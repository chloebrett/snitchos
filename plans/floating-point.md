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

1. **Register file + `fcsr`** — **DONE.** `Hart.f: [u64; 32]`, held as raw bit
   patterns rather than `f64` so a NaN payload we must preserve can't be
   canonicalised by the host on the way through. `f0` is an ordinary register —
   reusing `set_reg`'s index-0 special case would silently discard every write to
   it, so `set_freg` deliberately has no such branch (own test).
   `fcsr` is the **only stored FP CSR**: `fflags` and `frm` are *windows* onto
   `fcsr[4:0]` and `fcsr[7:5]` (`FcsrWindow`), because modelling them as three
   independent CSRs would lose a guest's rounding mode across a save/restore that
   went in via `frm` and out via `fcsr`.
   **`hash_state` had to grow `f`.** Easy to miss and silent: the snapshot tree
   resumes from those hashes and `snemu diff` compares them, so omitting FP state
   would let snapshot sharing resume with the wrong FP registers *and* make an FP
   divergence invisible to the differential oracle.
2. **Loads/stores + NaN boxing** — **DONE.** `flw`/`fld`/`fsw`/`fsd`, deliberately
   *not* routed through `load_value`/`store_value`: those sign-extend a 32-bit
   load, which is right for `lw` and wrong for `flw`. `flw` NaN-boxes (upper 32
   bits all ones); `fsw` writes only the low word, so the box never reaches memory.
   Tests observe results through a guest `ld` readback rather than host accessors
   (the `run_amo_d` idiom), so a store that never reached memory can't pass.
   Verified falsifiable, and the result is worth keeping: a zero-extending `flw`
   fails the boxing test but **still passes** the `fsw` round-trip test — which is
   precisely why this bug would stay hidden until something read the register as a
   double.
   Also added a **JIT on/off A/B** over FP loads/stores in a hot block. `compile_op`
   rejects FP via its catch-all, so blocks end before one — but that invariant is
   pinned rather than inherited, since a mis-compiled FP block is the failure
   `snemu diff` cannot audit (both sides would be snemu's own code).
   Still to do here: teach `expand` the compressed FP forms
   (`c.fld`/`c.fsd`/`c.fldsp`/`c.fsdsp`). Until then they report as snemu gaps,
   which is correct but will block real programs. Also, "reading an improperly boxed
   value yields canonical NaN" is an *arithmetic* rule — it lands with step 3, not
   here; step 2 only establishes the box on load.
3. **OP-FP** — **DONE.** Arithmetic (`fadd`/`fsub`/`fmul`/`fdiv`/`fsqrt`), sign
   injection, min/max, compares, `fclass`, `fmv` both directions, `fcvt` int↔float
   and between float widths — both precisions throughout. The semantics that differ
   from the host live in the pure, directly-tested `snemu/src/fp.rs`; `cpu.rs` only
   decodes and dispatches. Ordinary arithmetic is left to Rust's `f32`/`f64` — the
   reference is IEEE-754 and so is the host, so there is nothing to model.

   **The design doc's rounding-mode rule was wrong, and this is the correction.**
   The doc claimed `RNE` + `DYN`-while-`frm==0` "covers everything a Rust compiler
   emits". Measured with `rustc --target riscv64gc-unknown-none-elf --emit asm`:
   every float→int cast emits `fcvt.w.d a0, fa0, rtz`, because Rust's cast semantics
   are truncation. An RNE-only rule would have halted snemu at the first `as i32` in
   real guest code. So the gate is split by *who does the rounding*:
   `fp::arithmetic_rounding` (host FPU → nearest-even only, everything else refused
   loudly) and `fp::conversion_rounding` (snemu rounds it itself → `RNE` **and**
   `RTZ`). The refusal still names the *effective* mode, resolved through `frm`.

   Other divergences from the host, each with its own test:
   - **Canonical NaN** on any generated NaN — but *not* on `fsgnj`/`fmv`, which are
     bit moves and must preserve a payload.
   - **`funct3` is an op selector, not a rounding mode**, for sign injection,
     min/max, compares and `fclass`. Checking rounding for all of OP-FP refuses
     `fsgnjn` (selector 1 = `RTZ`) and so breaks `-x` — the failure mode is a halt on
     working code.
   - **`fmin`/`fmax`**: a lone NaN operand is *skipped* (not propagated), both NaN
     gives canonical NaN, and `−0.0 < +0.0` even though they compare equal — so
     `if a < b { a } else { b }` is wrong half the time on signed zero.
   - **NaN → maximum positive integer** on float→int, where Rust's `as` gives 0.
     LLVM emits an explicit `feq.d fa0, fa0` guard around every cast precisely to
     paper over this, which is the strongest available evidence for the hardware's
     behaviour.
   - **RV64 sign-extends every 32-bit FP→int result, `.wu` included**, so
     `fcvt.wu.d` of `u32::MAX` reads back as `-1`. `i64::from(u32)` zero-extends and
     was the one bug the tests caught in this slice.

   Small follow-up: `StepError::UnsupportedRoundingMode` reports the mode
   numerically (it reaches the user via `Debug`). The doc asked for it to be *named*;
   that needs a `Display` impl for `StepError`.
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

**On the oracle (decided 2026-07-26).** `snemu diff` can't give differential FP
coverage until the *kernel* enables FP (increment 4), because until then neither
side executes an FP instruction successfully. An S-mode FP probe workload would get
the oracle biting earlier — **rejected**: the kernel stays zero-FP, FP is a
userspace-only authority (same call as `vf2-audio-design.md`'s fixed-point choice),
and an `itest-workloads`-only exception isn't worth buying coverage that increment 4
provides anyway.

So 3b is built against **enumerated IEEE edge cases in host tests**, not a
differential sweep — which is the honest tool here regardless, since for ordinary
arithmetic the reference is IEEE-754 and Rust's `f64`, i.e. the same hardware QEMU
runs on. A diff would mostly confirm that `fadd.d` adds. The cases that genuinely
diverge from host behaviour are few and enumerable, and each wants its own test:
canonical NaN generation, NaN boxing, sNaN handling, `fmin`/`fmax` NaN rules,
`fcvt` saturation edges, `fclass`. Parity against QEMU arrives with increment 4, via
an FP-authorised *user* program.

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
