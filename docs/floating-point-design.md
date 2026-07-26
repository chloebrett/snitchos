# Floating point on SnitchOS

**Status:** 🚧 **IN PROGRESS** — increments 1 (unhandled U-mode trap kills the
process) and 2 (snemu delivers a guest illegal-instruction trap) are shipped; FP
itself is not. Sequence, per-increment tests and the corrections below live in
[../plans/floating-point.md](../plans/floating-point.md).

Two premises in this doc turned out to be wrong once the code was read, and are
corrected in the plan rather than silently here: problem (2)'s "as other user
faults do" — other user faults *parked the hart*, they didn't kill anything — and
problem (3), since snemu does print a guest kernel panic in the console tail of a
failing scenario. The original debug was blind because the scenario was
*unregistered*, so nothing failed and nothing dumped that tail.

**Prompted by a live bug.** `sstatus.FS` is never set,
so every floating-point instruction traps as illegal (`scause=2`) — and the
`UnknownException` arm panics the kernel regardless of privilege. Typing
`1.5 + 1.5` at the Stitch REPL takes down the machine. Found 2026-07-26 while
building REPL completion; see [../plans/repl-completion.md](../plans/repl-completion.md)
for the bisect.

Related: [vf2-audio-design.md](vf2-audio-design.md) (why the *kernel* is
zero-FP), [manifest-design.md](manifest-design.md) (where a declared-and-checked
requirement belongs), [snemu-design.md](snemu-design.md).

---

## Three separable problems

1. **Stitch's floats cannot run on target.** The language supports them fully;
   nothing on the metal had ever exercised one (every fixture — `primes.st`,
   the REPL demo — is integer-only).
2. **A user program can panic the kernel with one illegal instruction.**
   `kernel/src/trap/mod.rs`'s `UnknownException` arm panics even though the
   surrounding code already computes `from_user`. An unhandled *user* trap must
   kill the process, as other user faults do. **This is a robustness hole
   independent of floating point, and the more serious of the three.**
3. **snemu hides kernel panics.** Under snemu the guest merely stopped emitting
   frames; QEMU printed the panic. `panic-now` exists to assert that panics
   reach the wire, so this is a fidelity gap in its own right.

(2) and (3) should be fixed regardless of what is decided about FP.

## Decision: build FP into snemu (2026-07-26)

**snemu grows RV64F/D; soft-float is not taken.** The objection below —
"partial FP is a fidelity trap" — is weaker than it first looks, because
**`snemu diff` exists**: divergence from QEMU is detectable rather than silent,
which is precisely the tool that makes incremental FP safe. We own the
emulator; the answer is to make it model the machine.

Scope, so the corners are known up front:

- **The bulk is straightforward.** A 32 × 64-bit register file, `fcsr`, and
  ~60 opcodes that are mostly systematic width-variants of a few families.
  Loads/stores, arithmetic, sign-injection, compares and moves map directly
  onto Rust `f32`/`f64` — even float→int saturation agrees (Rust's `as` has
  saturated since 1.45).
- **NaN boxing.** Single-precision values in 64-bit registers must be
  upper-all-ones; reading an improperly-boxed value yields canonical NaN.
- **Canonical NaN.** RISC-V produces a canonical NaN rather than propagating
  the operand payload — host hardware does not.
- **Rounding modes and `fflags`** — decided, see below.

### How much of `fcsr` to implement — and the rule behind it

**A gap in snemu must never become guest-visible behaviour.** This week's bug
cost an hour precisely because snemu turned "I do not implement this" into
silence the guest experienced — and a gap that surfaces *as a guest trap* is
indistinguishable from a real hardware trap, so it sends you debugging the
kernel. A gap that surfaces as a **host-side panic naming the gap** costs
thirty seconds. That principle generalises past `fcsr` to every unimplemented
instruction, CSR and device.

Concretely:

- **Round-to-nearest-even is implemented fully.** Accept `rm = RNE (000)` and
  `rm = DYN (111)` while `fcsr.frm == 0` — which covers everything a Rust
  compiler emits (it emits DYN, and `frm` resets to 0).
- **Any other rounding mode panics on the host**, naming the mode, the PC and
  the instruction. Never a guest trap, never a rounded-anyway answer.
- **`fcsr` reads and writes work** as a plain register, so guest save/restore
  round-trips — but **accrued `fflags` are not computed**.

The asymmetry is deliberate, and it is about what the differential oracle can
see. A wrong *rounding mode* produces a plausible number that flows downstream,
so `snemu diff` would report a distant symptom long after the cause — those
gaps must be loud. A missing *flag* is different: if a guest ever reads and
branches on `fflags`, QEMU has them and snemu does not, so the divergence is
immediate and attributable. Gaps the oracle can catch may be lazy; gaps it
cannot must shout.

Two non-optional pieces:

- **The JIT** (`snemu/src/jit.rs`) must handle FP blocks or explicitly bail to
  the interpreter. A mis-compiled FP block is the one failure `diff` may miss,
  since both sides would be running snemu's own code.
- **`sstatus.FS` semantics** (Off/Initial/Clean/Dirty) plus the
  illegal-instruction trap when Off. Without these the *lazy-enable* design is
  untestable under snemu, which would defeat the purpose.

## Considered and not taken: soft-float userspace

Recorded because it was the initial recommendation and is a genuine fallback if
snemu FP stalls. The idea: rather than enable hardware FP, **stop emitting FP
instructions** — build the
userspace programs for a soft-float target (`riscv64imac-unknown-none-elf`, or
`riscv64gc` with the float features disabled), so the compiler lowers float
arithmetic to integer libcalls. Then:

- Stitch floats work on target, today;
- the kernel needs no change — `sstatus.FS` stays Off, `TaskContext` stays as
  it is, context switches stay cheap;
- snemu needs no change;
- the cost is slower float arithmetic, which nothing here cares about (and the
  audio path already chose fixed-point over FP for its own reasons).

The userspace programs are already built by a separate `cargo` invocation in
`kernel/build.rs`, so their target can differ from the kernel's without
affecting anything else — they never link together.

**Why it was not taken:** it makes the emulator's gap permanent policy. Floats
would work while the machine still cannot execute a float instruction, so the
first workload that genuinely wants FP (audio synthesis, graphics) would have
to unwind the decision. Modelling the machine is the honest fix, and the
differential oracle makes it safe to do incrementally.

## The real feature: opt-in hardware FP, derived not declared

If hardware FP is wanted later (a workload where float throughput matters),
**opt-in is right, and the opt-in should be *derived from the binary*, not
trusted from a declaration.**

- **RISC-V ELFs carry their float ABI in `e_flags`** (`EF_RISCV_FLOAT_ABI_*`).
  The loader can *read* whether a program uses FP. Unforgeable by accident, no
  new syscall, and — unusually for an authority claim — mechanically checkable
  rather than taken on faith. A manifest entry records it; the ELF proves it.
- **No new capability.** FP is not a scarce shared resource like the DAC: it is
  per-hart state that is always available. What opt-in buys is not arbitration
  but *cost attribution* — 32 FP registers is 256 bytes of save/restore on
  every context switch, and it is wrong to tax integer-only programs (which is
  nearly all of them) for a feature they never use.

**The mechanism is the fix for problem (2).** Rather than a flag checked at
spawn, do it lazily at the trap:

> On an illegal instruction from U-mode: if the process is FP-authorised and
> `sstatus.FS` is Off, enable FS, snitch, and retry the instruction. Otherwise
> kill the process with a structured refusal.

That is classic lazy-FP context switching, and here it doubles as the authority
check — the trap that currently panics becomes the enable-or-refuse decision
point. Integer-only programs never pay anything, and the robustness fix and the
feature land in the same place.

## Snitching on it

Consistent with the rest of the system, this should be observable rather than
implicit:

- an event when a process first enables FP — which process, and on what
  authority;
- a structured refusal when an unauthorised process attempts FP, before it is
  killed (refusals snitch, never silent);
- `snitchos.fp.processes_enabled` and `snitchos.sched.fp_saves_total`, so the
  context-switch cost of FP is *attributable* rather than diffuse.

"Which programs use floating point, and what does it cost us?" then becomes a
query, like every other authority question here.

## Order of work

1. **Fix (2)** — an unhandled *user* trap kills the process, never the kernel.
   Small, independent, and it closes a hole where any user program can halt the
   machine. Do this first regardless of everything else.
2. **Fix (3)** — snemu surfaces kernel panics, so the fidelity escape hatch is
   not the only way to see one. Cheap, and it is what made this bug take an
   hour instead of a minute.
3. **snemu FP** — register file, `fcsr`, decode/exec, `sstatus.FS` semantics,
   JIT handling or bail. Validate with `snemu diff` against QEMU as it lands.
4. **Kernel opt-in FP** — ELF-derived authority, lazy enable on the U-mode
   illegal-instruction trap, snitched. Testable under snemu once (3) is in.
5. Stitch floats then work on both engines, and
   `stitch-kvetch-completes` unblocks.

Note that (1) and (2) are worth doing even if FP never lands: one is a kernel
robustness hole, the other is an observability gap that hid it.

## Open questions

- Does anything in userspace depend on the hard-float ABI today (calling
  convention differences are ABI-visible: `lp64d` passes floats in FP
  registers)? All userspace crates are built together, so the switch must be
  wholesale — check `snitchos-user`, `snitchos-std`, `stitch`, `hitch`.
- Does the soft-float target change binary size enough to matter for the
  embedded ELFs (libcalls pull in compiler-builtins float routines)?
- Does the LLM runner's int8/fixed-point choice
  ([generative-ladder.md](generative-ladder.md)) relax once userspace FP
  exists? Its other reasons (memory bandwidth, no FP state on the hot path)
  stand on their own, so probably not — but it stops being *forced*.
