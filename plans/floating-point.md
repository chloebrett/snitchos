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
- **snemu cannot deliver `scause=2` at all.** `snemu/src/cpu.rs::cause` has no
  illegal-instruction code; an undecodable instruction becomes
  `StepError::Unimplemented { pc, instr }`, which halts the run host-side. That
  is already the design doc's "name the gap" rule, and it is *correct for a
  legal-but-unmodelled opcode* — but it means an instruction that is genuinely
  illegal for the guest (FP with `sstatus.FS == Off`) has nowhere to go. snemu
  needs both behaviours, discriminated.

## Increment 1 — an unhandled U-mode trap kills the process, never the kernel

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

New gate: a `workload=user-illegal` program that executes `unimp` from U-mode,
plus a scenario asserting the machine survives and the faulted process is
reaped. **Under QEMU only until increment 2** — snemu has no illegal-instruction
trap, so it halts the run instead of trapping the guest. Register the scenario
in the same increment as 2, not this one.

## Increment 2 — snemu distinguishes a guest-illegal instruction from its own gap

Two different meanings currently collapsed into `StepError::Unimplemented`:

- **A legal RV64GC instruction snemu does not model** → keep halting host-side
  naming pc + instr. This is the design doc's rule and it is already right.
- **An instruction that is illegal *for the guest*** — a genuinely invalid
  encoding (`unimp`), or (after increment 3) an FP instruction while
  `sstatus.FS == Off` → deliver a guest trap with `scause = 2`.

Needs a new `cause::ILLEGAL_INSTRUCTION: u64 = 2`, `stval = the instruction
word` (RISC-V sets `stval` to the faulting instruction for illegal-instruction
traps; check QEMU agrees via `snemu diff`), and a decision point that classifies
an encoding rather than "did my match arm fall through". The JIT (`snemu/src/jit.rs`)
must agree — a block containing an illegal instruction must trap at the right PC,
not at the block head.

Gate: register the increment-1 scenario on both engines; `snemu diff` against
QEMU on it.

## Increment 3 — snemu FP: register file, `fcsr`, decode/exec, `sstatus.FS`

Per the design doc's scope list: 32 × 64-bit `f` registers, `fcsr` as a plain
register (reads/writes round-trip; `fflags` not accrued), RNE and DYN-while-frm-0
only — **any other rounding mode host-panics naming the mode, the PC and the
instruction**. NaN boxing for single-precision. Canonical NaN on generation.
`sstatus.FS` Off/Initial/Clean/Dirty plus the illegal-instruction trap when Off
(built on increment 2). JIT handles FP blocks or explicitly bails to the
interpreter — a mis-compiled FP block is the one failure `diff` cannot see,
since both sides would be running snemu's code.

Validate with `snemu diff` against QEMU as each family lands.

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
