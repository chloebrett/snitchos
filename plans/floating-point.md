# Plan: floating point on SnitchOS

Design: [../docs/floating-point-design.md](../docs/floating-point-design.md). This
file is the increment sequence; the design doc carries the rationale and the
decisions (snemu grows RV64F/D; soft-float not taken; FP authority derived from
the ELF's `e_flags`, enabled lazily at the trap).

Two corrections to the design doc's premises, found while reading the code:

- **Other user faults do not kill the process — they park the hart.**
  `kernel/src/trap/mod.rs::handle_user_fault` is `loop { wfi }` with a stale
  v0.7a comment ("no process teardown"). There *is* teardown now
  (`sched::exit_now_owned`, `note_exit`, the REAP table). A U-mode page fault on
  hart 0 therefore stops the heartbeat. So increment 1 is not "route the new
  case into the existing kill path" — it is "make both paths kill".
- **snemu does not hide kernel panics — problem (3) is smaller than stated.**
  Measured 2026-07-26 by deliberately reverting increment 1 and re-running the
  new scenario under snemu: the failure output carried
  `Kernel panic: panicked at kernel/src/trap/mod.rs:211` in the console tail,
  because the harness dumps the guest UART on a failing scenario. So the panic
  *is* visible under snemu when a scenario fails. What the original debug session
  hit was different: `stitch-kvetch-completes` was **unregistered**, so nothing
  failed and nothing dumped the tail — the guest just stopped emitting frames and
  looked wedged. That's an unregistered-scenario blind spot, not a snemu fidelity
  gap. Before spending anything on increment 3, re-derive what is actually
  missing; on this evidence it may be nothing.
- **snemu cannot deliver `scause=2` at all.** `snemu/src/cpu.rs::cause` has no
  illegal-instruction code; an undecodable instruction becomes
  `StepError::Unimplemented { pc, instr }`, which halts the run host-side. That
  is already the design doc's "name the gap" rule, and it is *correct for a
  legal-but-unmodelled opcode* — but it means an instruction that is genuinely
  illegal for the guest (FP with `sstatus.FS == Off`) has nowhere to go. snemu
  needs both behaviours, discriminated.

## Increment 1 — an unhandled U-mode trap kills the process, never the kernel — **DONE**

The robustness hole, independent of FP: `trap_handler`'s `other => panic!(…)`
arm panics on any exception without a dedicated handler regardless of privilege,
so one illegal instruction from userspace halts the machine.

Pure, in `kernel-boot/src/trap.rs` (host-tested):

- `exception_name(code) -> &'static str` — the RISC-V exception names (0–7, 12,
  13, 15) for the snitch message, so a fault report says
  `illegal instruction`, not `scause=0x2`.
- `fault_disposition(cause, from_user) -> FaultDisposition` where
  `FaultDisposition = { TerminateProcess, KernelPanic }`. Rule: an *exception*
  with no dedicated handler from U-mode terminates the process; the same from
  S-mode panics; an unknown *interrupt* panics from either privilege (it is not
  attributable to the running process).

Kernel side, in `kernel/src/trap/mod.rs`:

- The `other =>` arm consults `fault_disposition` instead of panicking.
- `handle_user_fault` becomes `terminate_faulting_process(code)`: emit
  `snitchos.user.faults_total` (as now) plus a `Log` naming the exception,
  `sepc` and `stval`, then `note_exit(me, FAULTED_STATUS)` + wake the parent +
  `exit_now_owned()`. Modelled on `sched::exit_if_kill_requested`, with a
  distinct status (`FAULTED_STATUS`, sibling of `KILLED_STATUS = -9`) so a
  supervisor can tell a faulted child from a killed one.
- The existing `UnknownException(12 | 13 | 15)` user arm folds into the same
  path — kill, not park.

Existing gate: `userspace-cannot-touch-kernel` already asserts
`user.faults_total` then a following heartbeat, and does not depend on the
parking behaviour, so it should stay green and become a stronger assertion (the
heartbeat now proves the *faulting hart* recovered, not merely that hart 0 was
uninvolved).

New gate, shipped with increment 2 (which gave it a snemu engine):
`workload=userspace-illegal` runs `user/hello/src/bin/illegal.rs` — marker, then
`unimp` — and the `unhandled-user-trap-kills-only-the-process` scenario asserts
the marker, then a `Log` naming `user fault` by `illegal instruction`, then a
heartbeat *after* it. Green on **both** engines (snemu and `--engine qemu`), and
verified falsifiable: with the `TerminateProcess` arm temporarily reverted to
`panic!`, the scenario fails with the kernel panic in the console tail.

## Increment 2 — snemu distinguishes a guest-illegal instruction from its own gap — **DONE**

Two different meanings currently collapsed into `StepError::Unimplemented`:

- **A legal RV64GC instruction snemu does not model** → keep halting host-side
  naming pc + instr. This is the design doc's rule and it is already right.
- **An instruction that is illegal *for the guest*** — a genuinely invalid
  encoding (`unimp`), or (after increment 3) an FP instruction while
  `sstatus.FS == Off` → deliver a guest trap with `scause = 2`.

Shipped as `decode::is_guest_illegal`, scoped to the encodings the spec
*guarantees* illegal on every implementation — the all-zero and all-ones
instruction words — so no judgement about snemu's coverage is involved and the set
cannot rot as snemu grows. `cause::ILLEGAL_INSTRUCTION = 2`, `stval` = the
faulting instruction word, `sepc` un-advanced so a handler may retry the same PC
(which is how lazy FP enable will work). Illegal words are deliberately **not**
entered into the fetch cache, for the same reason.

`fetch_for_compile` declines them too, so a hot block ends there and the
interpreter delivers the trap. That is load-bearing rather than defensive: a
mis-compiled block is the one failure `snemu diff` structurally cannot see, since
both sides would be running snemu's own code and QEMU never gets a look in — so
its test is an internal JIT-on/JIT-off A/B on the resulting trap state.

**This found a live fidelity bug.** All-zeros was *executing*: it reached the
compressed path, `expand` accepted it as `c.addi4spn` with a reserved zero
immediate, and the pc advanced by 2. Exactly the "a gap becomes guest-visible
behaviour" failure the design doc's rule is aimed at.

Both new snemu tests are behavioural (through `step()`), and the
legal-but-unmodelled path keeps its own test — see 3a, which moved its witness to
`mret` once the FS gate started (correctly) trapping `fadd.d`.

## Increment 3 — snemu FP: register file, `fcsr`, decode/exec, `sstatus.FS`

### 3a — the `sstatus.FS` gate — **DONE**

Taken first because it is what increment 4 hangs off, and because on its own it
makes snemu faithful to *today's* kernel without needing an FP unit at all: with
`FS == Off`, every FP instruction is illegal, so modelling the refusal is the whole
behaviour. Before this, snemu had no illegal-instruction cause and halted the run
host-side, so the kernel's FP behaviour was untestable there.

- `sstatus::FS` (bits 14:13) + `FS_INITIAL` in `snemu/src/csr.rs`. snemu
  distinguishes Off from not-Off and deliberately does **not** track the
  Clean/Dirty transitions yet — nothing reads them until the kernel enables FP.
- `decode::is_fp_instruction` covers all seven FP opcode families (`LOAD-FP`,
  `STORE-FP`, `MADD`/`MSUB`/`NMSUB`/`NMADD`, `OP-FP`) plus a `csr*` naming
  `fflags`/`frm`/`fcsr`. Classified by *opcode family*, not by "what snemu
  implements" — the gate must refuse an FP instruction snemu has no unit for
  exactly as hardware would.
- The check sits at the top of `execute()`, so one site covers the decode-cache
  hit and the compressed expansion (`c.fld` → `fld`), and it runs **before**
  dispatch. Order matters: with FS on, an unimplemented FP opcode reports snemu's
  gap host-side; with FS off it traps the guest. Backwards, and every FP gap looks
  like hardware refusing FP.

Three now-distinct cases, each with its own test: guest-illegal encoding → guest
trap; FP with FS off → guest trap; legal-but-unmodelled → host halt. That last
test's witness had to move off `fadd.d` (which the new gate correctly traps) onto
`mret` — a non-FP legal-but-unmodelled instruction — so it still proves the general
rule rather than the FS gate.

**Payoff, gated:** `stitch-float-does-not-kill-the-kernel` types `1.5 + 1.5` at the
REPL and asserts the machine survives — the originally-reported bug, now a
regression test, green on both engines. Verified falsifiable: with `1 + 1` injected
instead the REPL prints `=> 2`, no fault Log appears, and the scenario fails. It is
deliberately a **tripwire for increment 4**: when lazy enable lands, `1.5 + 1.5`
should print `3` and this scenario must be rewritten. If increment 4 lands and it
still passes unchanged, the lazy enable isn't reaching the REPL.

### 3b onward — the FP unit itself

Not started. Per the design doc's scope list, in a sensible landing order — each
step is independently testable, and the FS gate above means none of them changes
observable behaviour until the kernel enables FP:

1. **Register file + `fcsr`.** 32 × 64-bit `f` registers; `fcsr` as a plain
   register that round-trips (so guest save/restore works) with `fflags` **not**
   accrued. Add `fflags`/`frm`/`fcsr` to the modelled CSR set — note they are
   currently gated by `is_fp_instruction`, so they trap before reaching the CSR
   file, and the FS-on path needs them present.
2. **Loads/stores** (`flw`/`fld`/`fsw`/`fsd`) + **NaN boxing**: a single-precision
   value in a 64-bit register must be upper-all-ones, and reading an improperly
   boxed one yields canonical NaN. Also teach `expand` the compressed FP forms
   (`c.fld`/`c.fsd`/`c.fldsp`/`c.fsdsp`) — until then they report as snemu gaps,
   which is correct but will block real programs.
3. **OP-FP**: arithmetic, `fsqrt`, min/max, sign injection, compares, converts,
   moves, `fclass`. Mostly systematic width-variants over Rust `f32`/`f64`; even
   float→int saturation agrees (Rust's `as` has saturated since 1.45). Two
   deliberate divergences from host hardware to get right: **canonical NaN** (RISC-V
   generates one rather than propagating the operand payload) and the rounding-mode
   rule — accept `rm = RNE` and `rm = DYN` while `fcsr.frm == 0` (all a Rust
   compiler emits), and **host-panic on any other mode**, naming mode + PC +
   instruction. A wrong rounding mode yields a plausible number that diverges far
   downstream, so that gap must shout.
4. **FMA** (`fmadd`/`fmsub`/`fnmsub`/`fnmadd`), which need a fused multiply-add —
   `f64::mul_add`, not `a * b + c`, or the intermediate rounds twice.
5. **`sstatus.FS` Clean/Dirty transitions**, once the kernel has something that
   reads them (an FP write sets Dirty; that's what lets a context switch skip
   saving unmodified state).
6. **JIT**: handle FP blocks or explicitly bail to the interpreter. Same reasoning
   as increment 2's `fetch_for_compile` — a mis-compiled FP block is the one
   failure `snemu diff` cannot see, since both sides would be running snemu's own
   code, so it needs an internal JIT-on/JIT-off A/B rather than the differential
   oracle.

Validate with `snemu diff` against QEMU as each family lands — but note the oracle
only bites once the *kernel* enables FP (increment 4), since until then no guest
program executes an FP instruction successfully on either side. Consider landing a
small S-mode FP probe workload to get differential coverage earlier.

## Increment 4 — kernel opt-in FP, derived from the ELF, enabled lazily

- The loader reads `EF_RISCV_FLOAT_ABI_*` out of the ELF's `e_flags` and records
  FP authority on the process. Pure ELF-header work → `kernel-proc/src/elf.rs`,
  host-tested.
- On an illegal instruction from U-mode: if the process is FP-authorised and
  `sstatus.FS` is Off, enable FS, snitch, and retry the instruction (do *not*
  advance `sepc`). Otherwise fall through to increment 1's terminate path. This
  replaces increment 1's arm for `scause=2` specifically; every other unhandled
  exception still terminates.
- `TaskContext` grows the 32 FP registers, saved/restored **only** for tasks
  whose FS is not Off — the whole point of the opt-in is that integer-only tasks
  do not pay the 256 bytes.
- Snitch it: an event on first enable (which process, on what authority), a
  structured refusal before killing an unauthorised process,
  `snitchos.fp.processes_enabled` and `snitchos.sched.fp_saves_total`.

## Increment 5 — Stitch floats work on target

`1.5 + 1.5` at the REPL evaluates instead of taking down the machine, and
`stitch-kvetch-completes` registers (its oracle probes the `Float` class). See
[repl-completion.md](repl-completion.md).

## Open questions carried from the design doc

- Does the hard-float ABI (`lp64d` passes floats in FP registers) already reach
  any userspace crate? All of `user/` builds together, so an ABI switch would be
  wholesale — but increments 3–4 keep hard-float, so this only matters if the
  soft-float fallback is ever taken.
- Does `stval` on an illegal-instruction trap carry the instruction word on
  QEMU, on snemu, and on the JH7110 U74s? The spec permits 0.
