# Plan: FP context switching (floating-point.md increment 4b)

**Branch**: main (house rule — no feature branches)
**Status**: ✅ **COMPLETE (2026-07-28).** All six steps landed. Two processes run
hardware FP concurrently, `RefuseBusy` is gone, and `stitch-kvetch-completes` is
registered and green on both engines. snemu 130/130 plain and `--scramble`.

### What the plan did not predict

**Firmware does not hand the kernel `sstatus.FS = Off`.** Step 5's negative oracle —
the boring half, "an integer-only boot saves nothing" — failed on the first QEMU run
with `context_saves_total = 2`, one spurious save per hart. OpenSBI leaves `FS` set;
snemu resets it to `Off`. Harmless in itself, but the same assumption is load-bearing
somewhere that is not harmless: a user task inherits `FS` from the kernel context that
enters it, so on a machine whose firmware leaves the unit on, **userspace would get FP
as ambient authority** — no trap, no `enable_decision`, no `Log` naming the grant. The
lazy-enable design silently depended on a fact nobody had checked. Fixed by
`sched::fp_init_hart()`, called per hart in `kmain` and secondary bring-up.

Worth noting *which* engine found it. snemu resets `FS` to `Off`, which is faithful to
the hardware reset value — it is not an snemu bug, it is a scope boundary: snemu models
a machine, not a firmware handoff. The VF2 boots through OpenSBI too, so this was a
real-board bug that only the QEMU path could surface. The kernel no longer depends on
either behaviour, which is what makes the difference stop mattering.

**And the step it validated:** the negative oracle was the cheap half of step 5 and the
only reason this was found. A feature test proves the register file is carried when it
must be; only the negative proves it isn't carried when it needn't be — and that is the
assertion that noticed the FP unit was already on.

## Goal

Two processes can execute hardware floating point at the same time without
corrupting each other, so `FpEnableDecision::RefuseBusy` — and the dead server it
currently produces — can be deleted.

## Why now

`RefuseBusy` was written as a guard against a case nobody had, on the reading that
"today only the REPL uses FP, so nothing hits it". REPL Tab completion breaks that
reading **structurally**: the Stitch oracle runs on *both* sides of the wire — the
kvetch server samples a completion through it, and `ModelCompleter` re-validates the
suggestion through it, because a client must not trust another process to police its
own output. Two lexers means two processes that will parse a float literal
(`babble`'s `FLOATS` → `TokenKind::Float(f64)` → `dec2flt`). No ordering satisfies a
one-holder rule. Measured 2026-07-28: tab 1 completes and is inserted at the prompt,
tab 2 kills the server:

```
fp refused: task 4 — another process holds the FP registers …
user fault: task 4 killed by illegal instruction (scause=2 sepc=0x100280bc)
```

## The two design calls, and why

### 1. Save at the **switch**, not at the trap

The kernel is zero-FP by design. Nothing between trap entry and a context switch can
disturb a task's FP registers — only *another task running FP* can. So the save
belongs in `switch`, not in `TrapFrame`.

The alternative (grow `TrapFrame` by 33 words) would pay 264 bytes of save/restore on
**every** syscall and **every** timer tick, including the overwhelming majority that
return to the same task. Saving at the switch pays only when the scheduler actually
changes tasks, which is the population the cost belongs to. This also keeps the
change out of the trap entry/exit asm, which every scenario in the suite depends on.

### 2. All 32 registers + `fcsr`, **not** just `fs0`–`fs11`

`switch` saves only `s0`–`s11` today because it *is* a function call: the C ABI lets
the compiler treat caller-saved registers as clobbered across it. Transferring that
reasoning to FP would save `fs0`–`fs11` and stop. **That is wrong, and preemption is
why.** A task preempted by the timer never made a call — no instruction boundary in
it is a clobber point — so `ft0`–`ft11` and `fa0`–`fa7` must survive too.

The integer side gets this for free because `TrapFrame` already saves all 31 GPRs at
trap entry. FP has no such frame (see call 1), so the full set lives in the task.

`fcsr` goes with them: it holds the rounding mode and accrued exception flags, and a
task that sets a rounding mode must not have it silently changed by another task's
arithmetic. (snemu models `fflags`/`frm` as windows onto `fcsr`, so one register
covers all three.)

### 3. `sstatus.FS` is the filter, and it must stay the lazy-enable trigger

Save only when the outgoing task's live `FS` is `Dirty` — it has written FP since its
last restore. `Clean` means the in-memory copy is still valid (skip the save),
`Off` means the task has no FP state at all. This is exactly what the Clean/Dirty
tracking from increment 3b step 5 exists to make cheap.

Two hazards to hold onto:

- **The copy code itself needs `FS != Off`** — `fsd`/`fld` are FP instructions. The
  switch has to raise `FS` around the copy and set the *final* value afterwards.
- **A task with no FP must be switched in with `FS = Off`**, or its first FP
  instruction stops trapping and the lazy authority check in `kernel::trap::fp`
  silently stops running. The enable path is only reachable while `FS` is `Off`.

## Acceptance criteria

- [ ] Two userspace processes each running FP concurrently compute **correct**
      results — verified by value, not by absence of a crash.
- [ ] A process that has never executed an FP instruction still traps into the
      authority check on its first one (lazy enable is not regressed).
- [ ] `FpEnableDecision::RefuseBusy`, `FP_HOLDERS` and the `release` hooks on both
      death paths no longer exist.
- [ ] A context switch that carries FP state is observable on the wire, separately
      from one that does not.
- [ ] `stitch-kvetch-completes` is registered and passes on **both** engines, and a
      REPL session survives repeated Tabs.
- [ ] `cargo xtask test && cargo xtask itest && cargo xtask itest --scramble` green.

## Steps

Every step follows RED-GREEN-MUTATE-KILL MUTANTS-REFACTOR. No production code
without a failing test.

### Step 1: The switch policy, as a pure function

**Acceptance criteria**: Given the outgoing task's `FS` and whether the incoming task
owns FP state, `kernel_proc::fp` decides save/skip and restore/skip, and says what
`FS` the incoming task resumes with. `Dirty` saves; `Clean` and `Off` do not; an
incoming task with no FP state resumes `Off` (so lazy enable still fires); an
incoming task whose registers were restored resumes `Clean`. Host-tested in
`kernel-proc`, no `unsafe`, no asm.
**RED**: `switch_action(outgoing_fs, incoming_owns_fp)` cases, one test per row of the
table above — including the two hazards named as their own tests (`Off` in ⇒ `Off`
out, so the trap still fires).
**GREEN**: The enum + match.
**MUTATE**: `cargo xtask mutants -p kernel-proc`.
**KILL MUTANTS**: Any surviving arm means a case the table does not pin.
**REFACTOR**: Fold with `enable_decision` only if they genuinely share a shape.
**Done when**: criteria met, human approves commit.

### Step 2: `TaskContext` carries an FP register file

**Acceptance criteria**: `TaskContext` gains 32 FP registers + `fcsr`; the offsets
`sched.S` assumes are asserted against the Rust layout at compile time, so the two can
never drift silently. No behaviour change yet — the asm does not touch the new fields.
**RED**: A layout assertion that fails against a deliberately wrong offset (the
existing integer offsets get the same treatment while we are here — they are
load-bearing and currently pinned only by a comment).
**GREEN**: The fields + `const` offset assertions.
**MUTATE**: n/a for a layout assertion; note why in the commit.
**REFACTOR**: —
**Done when**: full suite still green (this step is inert by construction).

### Step 3: `switch` saves and restores under the policy

**Acceptance criteria**: Two kernel-side contexts with distinct FP register contents
survive a round trip through `switch` with their values intact. This is the risky
step — it edits the asm both `switch` and `switch_into` share.
**RED**: An itest (`workload=fp-two-tasks`) in which two userspace tasks each fill the
FP registers with a distinguishable pattern, yield/preempt repeatedly, and emit a
checksum of what they read back. The scenario asserts the checksums, so a clobber is a
*wrong number*, not a silence. **Verify falsifiability**: comment out the save, watch
it fail.
**GREEN**: Save/restore in `sched.S`, gated on the step-1 policy passed in from Rust.
`switch_into` needs the restore half only (the outgoing task is gone).
**MUTATE**: Not meaningful for asm; the falsifiability check above stands in for it.
**REFACTOR**: —
**Done when**: the new scenario passes on both engines, plain and `--scramble`.

### Step 4: Delete the interim guard

**Acceptance criteria**: `RefuseBusy`, `FP_HOLDERS`, `fp::release` and both death-path
calls are gone; `enable_decision` loses its `other_holders` argument; the refusal path
that remains is `RefuseUnauthorised` only. No process is ever refused FP for being
second.
**RED**: The `kernel-proc` tests for `RefuseBusy` are *deleted*, and a test asserting
two authorised processes both get `Enable` replaces them.
**GREEN**: Remove the variant and its plumbing.
**MUTATE**: `cargo xtask mutants -p kernel-proc`.
**REFACTOR**: —
**Done when**: criteria met; `snitchos.fp.refused_total` still fires for the
unauthorised case.

### Step 5: The cost is on the wire

**Acceptance criteria**: `snitchos.fp.context_saves_total` and
`snitchos.fp.restores_total` distinguish a switch that carried FP state from one that
did not — so "which processes make switches expensive" is answerable from the wire,
which is the whole argument the lazy design rests on.
**RED**: A scenario asserting saves stay at zero while only integer tasks run, then
climb once an FP task is scheduled.
**GREEN**: Two counters, bumped where the policy decides.
**MUTATE**: —
**REFACTOR**: —
**Done when**: criteria met.

### Step 6: Register `stitch-kvetch-completes`

**Acceptance criteria**: The scenario is registered in `SCENARIOS` and passes on both
engines; the `#[allow(dead_code)]` and the explanatory comment in `itest.rs` are gone.
A REPL session survives repeated Tabs rather than wedging on the second.
**RED**: Register it — it is already written and currently fails for the FP reason.
Extend it to press Tab **twice**, since one Tab never reproduced the bug.
**GREEN**: Nothing, if steps 1–4 are right. If not, this is where we find out.
**MUTATE**: —
**REFACTOR**: Fix the two defects found on 2026-07-28 while here, or split them out:
`RuntimePlatform::complete` re-registers its counter on every Tab (per-process quota
is 16, no dedup, so it silently stops counting after ~13) and emits a constant `1`
rather than a running total.
**Done when**: full gate green, `plans/repl-completion.md` closed out.

## Known unknowns

- **Is FP state per-task or per-process? — STILL OPEN, and now load-bearing.**
  `fpswitch::current_owns_fp` reads `Process::fp_enabled`, which is per-*process*,
  while the registers belong to an execution *context*. These are the same thing only
  while each process has exactly one task, which is true today and was not verified.
  If a process ever gets two tasks, both will claim to own one register file and each
  will restore the other's saved state. The fix is a flag on the `Task`; the reason it
  was not done now is that setting it lives in `try_enable`, which runs in trap context
  and would have to reach the scheduler lock to find the current task.
- **Where `FS` lives across the trap.** For a user task the value that reaches U-mode
  comes from `frame.sstatus` on `sret`, not from the live CSR, so `FS` already follows
  the task via its own frame. The switch manipulates the live CSR for the copy and for
  the *next* save decision. Worth a test that pins which of the two is authoritative,
  because getting it backwards is the kind of bug that only shows under preemption.
- **SMP.** FP registers are per-hart; task-owned save/restore is correct across harts
  by construction, but the `--scramble` and 4-hart scenarios are the check.

## Pre-PR quality gate

1. Mutation testing on `kernel-proc` (steps 1 and 4).
2. Falsifiability check on step 3 (break the save, watch the scenario fail).
3. `cargo xtask clippy` — whole workspace, host + riscv.
4. `cargo xtask test && cargo xtask itest && cargo xtask itest --scramble`.
5. `cargo xtask links` if any doc moved.

---
*On completion, `git mv` this file to `plans/legacy/` (house override of the planning
skill's "delete when complete").*
