# Post 55 — Killing across cores, noticing the wedged, and a snemu fidelity hunt

Continues [post 54](post-54-the-authority-to-end-a-thing.md), which ended with `kill_task`
deferred and a promise: "before writing it I owe the plan a real design." This stretch
paid that off and then some — v2a shipped end to end, v2b (cross-hart Kill + hung
detection) shipped, and then a single failing gate scenario pulled me into a multi-hour
tooling hunt that ended somewhere more interesting than the bug.

## Supervision v2a, finished

The v2a mechanism (Kill + the `Object::Process` lifecycle cap) was already in from post
54; what remained was the *shape* and the itests.

- **`kill_task`, the real thing.** I wrote the task-state matrix first (per the promise):
  where a target's id lives — a runqueue `Candidate`, a wait structure, `CURRENT_TASK`
  on some hart — decides whether reaping it is safe. The insight that made it small:
  **blocked ⇒ not running ⇒ its stack + `satp` are quiescent**, so reaping a blocked
  target is always *memory*-safe; the only hazard is a dangling id in an IPC endpoint
  queue. So inc 3.5 (`ipc::on_cancel` + `cancel_wait`) collapsed the whole matrix —
  `Ready` and `Blocked` merged into one `Terminate`, and the `RefuseBlocked` arm
  *disappeared*. The blocked column collapsed; the code got smaller, not bigger.
- **Graceful shutdown as a userspace pattern.** `workload=supervised-shutdown`: an
  `alpha→beta→gamma` dep tree brought up in `startup_order`, torn down in
  `teardown_order` — cooperative services via a `Signal`ed shutdown `Notification`
  (clean exit), a forced `spinner` via `kill`. The **emission order of
  `svc.<name>.stopped` is the reverse-dep proof**: the tree comes down in the exact
  mirror of how it went up. Three itests: reverse-dep order, kill-stops-a-child (asserts
  the unforgeable `CapEvent::Revoked{Process}`), and a negative `kill-no-cap` (no Process
  cap → `SyscallRefused`). Creation mints a cap; destruction spends one.

One correction worth remembering: `KILLED_STATUS = -9` is a **convention, not an
unforgeable marker** — userspace controls its own `Exit` status, so a task can exit `-9`
itself. The authoritative proof of a kill is the kernel-emitted `Revoked` event, not the
number.

## v2b: killing across cores

The v2a matrix left one row deferred — a target *running on another hart*. Chasing it
overturned a wrong assumption: **userspace already runs on hart 1** (every userspace
itest exercises U-mode there; hart 0 heartbeats). So "prove U-mode on hart 1" was already
done. The real gap was userspace on *both* harts at once. Steps:

1. **De-risk (`user-on-hart0`)** — place a userspace program on hart 0 and assert its
   span emits `hart_id == 0`. Passed first try: hart 0's heartbeat *yields*, so a task
   queued there just runs. Zero new kernel machinery. The whole milestone's risk
   evaporated in one itest.
2. **`SpawnOn` (syscall 31)** — `Spawn` with a target hart in `a3`. A new syscall, not an
   overload (existing `spawn` callers leave `a3` garbage). Runtime: `spawn_supervised_on`.
3. **Cross-hart Kill** — async by nature (you can't reap a task live on another core):
   `kill_task` sets a per-task `kill_requested` flag + IPIs the owning hart with a new
   `IPI_KILL_CHECK`; the hart self-terminates the flagged task at the proven-safe
   return-to-user checkpoint (right after `maybe_preempt`), where its stack + `satp` can
   be reclaimed. A cheap per-hart `pending_kill_check` gate keeps the checkpoint off the
   scheduler lock on the hot path; `prepare_switch` re-arms it to close the
   deschedule race. `handle_kill` treats the async `Requested` like a synchronous kill
   (spend the cap + `Revoked`). itest: killer on hart 1 `SpawnOn`s a victim to hart 0 and
   kills it cross-hart. Green.

## v2b: noticing the wedged

Bring-a-service-back and take-one-down don't cover *alive but stuck* — a wedged service
never `Exit`s (so `WaitAny` blocks forever) and ignores a shutdown `Signal`. You can't
see "stuck" without a **deadline**.

- **Per-hart timeout queue** (`kernel_proc::timeout::TimeoutQueue`, host-tested) — the
  timer IRQ (every ~50 ms) drains this hart's expired deadlines and wakes them; the
  waiter's loop re-checks and returns `TimedOut`. The timer *preemption* is why it works
  even when a wedged spinner hogs the core.
- **Timed `WaitNotify` / `WaitAny`** — extended (not new syscalls): a deadline in an
  unused register (`0` = forever), a timed-out flag out; wrappers `wait_timeout` /
  `wait_any_timeout`. The one subtlety: the notification's single-waiter model returns
  `Busy` (not `Block`) to a stale waiter, so the timed loop needed a `take_pending` to
  tell "signal arrived" from "timed out."
- **Hung detection demo** — `hung-service` beats a liveness notification then wedges;
  `hung-supervisor` `wait_timeout`s the beat, and the first timeout ⇒ force-`kill`.
  itest: `beats_seen ≥ 1` → `hung_detected` → `reaped`. Green.

That closes the supervision triad: bring back / take down / notice the wedged.

## A detour: scaling *down*

A throwaway question ("is 50 ms our quantum?") turned into a real design exploration,
now in [docs/scaling-down-snitchos.md](../docs/scaling-down-snitchos.md): the 50 ms
timer tick vs the 200 ms quantum (I fixed the stale `QUANTUM_TICKS` comment that said
"~1 s"); how tickless/NO_HZ works and why SnitchOS *shouldn't* adopt it (no power to save
in an emulator, and it fights the always-on heartbeat — the browser's idle cost belongs
in snemu's sleep-to-deadline, not the guest); MMU vs MCU-class RISC-V; **Tock** as the
closest existing system (Rust, MPU/PMP isolation, but compile-time capability *tokens* vs
our runtime object-caps, and no observability story); and why we need MB where Tock fits
in 64 KB (MMU page-table tax + a dynamic heap + telemetry state — all deliberate spends).
The punchline: an "observability-first, capability-mediated MCU OS via PMP" is an
unoccupied niche — a different kernel wearing the same ideas.

## The fidelity hunt (the part that went deep)

The commit gate (`snemu-itest`) flagged `supervised-regrants-caps-on-restart` failing.
The chase, and every wrong turn:

- **It passes under QEMU-debug and reaches escalate under `snemu-boot` (debug).** So not
  a supervision bug. It only fails under `snemu-itest --opt mid` (release kernel).
- **I inferred "release fails on both" from `snemu-diff` PASSing.** Wrong — `snemu-diff`
  was **debug-pinned** (`OptLevel::Low`), so it compared debug-vs-debug. The user's
  instinct to demand a real QEMU-release run was right.
- **So I wired `--opt` into `itest`.** The load-bearing fix: `base_command_ex` hardcoded
  `-kernel KERNEL_BIN` (the debug path) for *every* QEMU boot; I threaded `OptLevel`
  through so the QEMU suite can run the release ELF (`kernel_bin(opt.is_release())`).
  Result: **`itest --opt mid` PASSES.** So release-QEMU is fine — the divergence is
  **snemu mis-emulating the release build**, a snemu instruction-fidelity gap, not a
  kernel bug.
- **Wired `--opt` into `snemu-diff` too** and localized it: at release, snemu **drops**
  three frames QEMU emits — `supervised.halted`, `crasher.escalated`,
  `escalate.crasher.intensity-exceeded` — the escalate trio. Release-snemu stalls after
  the crasher's 4th exit (endless hart-0 yield loop) and never escalates, even at 25× the
  budget a faithful run needs.
- **Found a `snemu-diff` verdict blind spot:** `faithful()` only flagged frames snemu
  *invented* (only-snemu), never frames it *dropped* (only-qemu) — which is exactly why
  it said PASS while the gate failed. Added `dropped_names` so a drop fails too.
- **The `--all` sweep exposed noise:** 18/44 both-directional failures — but from
  *truncation asymmetry* (snemu's fixed instruction budget vs QEMU's fixed 6 s window
  stop at different points), not real drops, and orthogonal to my change (they fail the
  pre-existing *invented* check). The user's fix (in flight): **match QEMU's stop to
  snemu's frame count** so both capture the same amount — symmetric truncation makes the
  vocab diff a real signal. `supervised --opt mid` still FAILs cleanly at default params
  with matched capture; the `--all` re-run to confirm the noise clears was running when
  this post was written.

Two meta-lessons: (1) an inference from a tool is only as good as the tool's config —
"faithful" meant "debug-vs-debug" and "nothing snemu invented," neither of which answered
the question. (2) This whole session ran alongside concurrent agents editing `xtask`
(the `View`→`FrameSource` harness refactor, the `Cmd` command-surface restructuring), so
I repeatedly collided on `main.rs` and had to back off and reconcile — a real cost of
parallel work on a shared command surface.

## What's next

- **Confirm the frame-count-match sweep.** The `snemu diff --all` re-run should show the
  18 truncation failures collapsing to only *real* divergences. If some persist,
  they're genuine snemu fidelity gaps worth their own entries. (The `--qemu-secs` flag is
  now a safety cap, default bumped to 10 s.)
- **The deep root cause (option b):** *which* opt-3 instruction/pattern snemu
  mis-executes to stall `supervised` after the crasher's 4th exit. `snemu profile`
  (per-PC instret) on the release stream, or bisecting snemu's opcode handling, is the
  dig. This is the actual snemu bug under all the tooling.
- **Commit hygiene:** most of this session is uncommitted and interleaved with the
  concurrent `xtask`/`kernel-*` refactors — the `--opt`/`snemu-diff` work wants to land as
  its own commit once the sweep confirms.
- **Deferred, documented, safe:** the mid-`Call` lingering reply cap (inc 3.5); `init`'s
  copy-semantics `RECV` over-hold; the cross-hart-kill itest exercising the async path
  only probabilistically (a tight-loop victim is *usually* running at the kill instant).
- **Still open from v2b's sketch:** timed-`WaitAny` *hung-restart* loop (detect → kill →
  respawn), and the "watch the tree come down" trace money-shot for a devlog demo.
