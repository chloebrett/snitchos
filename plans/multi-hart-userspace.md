# Multi-hart userspace → cross-hart Kill (v2b enabler)

Supervision v2a deferred one row of the `kill_task` matrix: a target **running on
another hart**. v2b's cross-hart Kill fills it — but it has *no consumer* until
userspace actually spans two harts, so this milestone builds that first.

## What's already true (corrected premise, 2026-07-17)

Exploring for v2b overturned my initial assumption that userspace is single-hart:

- **Userspace already runs on hart 1.** `kmain` places every userspace layout program
  on hart 1 (`user::spawn_program(1, …)`, `main.rs:589`) while hart 0 runs the
  heartbeat. So U-mode on hart 1 is proven — every userspace itest (incl.
  `supervised-shutdown`) exercises it. "Prove U-mode on hart 1" is **done**.
- **Hart 0's heartbeat loop yields** (`heartbeat::run` → `sched::yield_now()`,
  `heartbeat.rs:160,184`), so hart 0 can run tasks queued on it between ticks — it is
  not a tight loop that monopolises the core.
- **Spawn-on-a-hart plumbing exists**: `spawn_process_with_caps(hart, …)` →
  `spawn_on_with_arg`. Userspace `Spawn` just hard-codes `current_hartid()` (= 1).

So the gap is **userspace on *both* harts simultaneously**: a userspace child on hart 0
while its supervisor is on hart 1. That cross-hart relationship is what gives
`kill_task`'s `running_remote` case a real, itestable consumer.

## Increment breakdown

### Step 1 — prove a userspace process runs on hart 0 (de-risk) ✅

**Result: passed on the first try — U-mode works on hart 0 with no new kernel work.**
The `user-on-hart0` workload places `hart-probe` on hart 0 (`user_hart` in the `kmain`
launcher); it enters U-mode, opens a span, and the `SpanStart` reached the wire tagged
`hart_id == 0`. Itest `userspace-runs-on-hart-0` green (max wait 0.2s). So the milestone
foundation is solid: userspace runs on **both** harts. Steps 2–4 are now unblocked and
should be quick. Shipped: `WorkloadKind::UserOnHart0` + parse (host test), `hart-probe`
bin, ELF/ProgramSpec/LAYOUTS wiring, launcher `user_hart` placement, itest.

Original plan:

The whole milestone's risk is concentrated here: hart 0 has only ever run kernel tasks
+ the heartbeat, never U-mode. The machinery is per-hart (`CURRENT_PROCESS.this_cpu()`,
per-hart `satp`, `stvec` set since boot), so it *should* work — hart 0 is the
fully-set-up boot hart, arguably lower-risk than hart 1 was. But it's unrun.

- Add a `hart: usize` field to `ProgramSpawn` (currently name/program/priority);
  the launcher uses `spawn_program(p.hart, …)` instead of the hard-coded `1`. Existing
  rows become `hart: 1` (unchanged behaviour). This is also the step-2 mechanism.
- New workload `user-on-hart0`: one trivial userspace program placed on **hart 0**.
- The program opens a distinctive span (a `SpanStart` frame is emitted from the
  syscall handler *on the running hart*, so it carries `hart_id`); assert a
  `SpanStart{name, hart_id == 0}`. If hart-0 U-mode is broken, the frame never appears.
- **Outcome sizes the rest**: if it just works, steps 2–4 follow quickly; if not, it
  surfaces exactly what hart-0 U-mode needs (sscratch/exception-stack/satp/codegen).

### Step 2 — hart-pinned userspace Spawn ✅

Shipped a dedicated **`SpawnOn` syscall** (= 31; `a3` = target hart) rather than
overloading `Spawn` (existing callers leave `a3` as garbage). Same `a0`/`a1`/`a2` +
returns as `Spawn`; an out-of-range hart refuses. `handle_spawn`/`handle_spawn_on`
share a `spawn_registry(frame, sc, hart)` inner. Runtime: `spawn_supervised_on(program,
handles, hart) -> Option<Child>`. The child lands on the target hart's runqueue + gets
an `IPI_WAKEUP` (via `spawn_on_with_arg`).

### Step 3 — cross-hart Kill ✅

Shipped as designed:
- `Task.kill_requested: AtomicBool`; `PerHartData.pending_kill_check: AtomicBool` (the
  cheap gate, placed *after* `exc_stack_top` to keep its hardcoded offset).
- `IPI_KILL_CHECK` (1 << 2): its handler just arms this hart's `pending_kill_check`.
- `kill_task`, `running_remote`: set `target.kill_requested` (Release) under the lock +
  `ipi::send(hart, IPI_KILL_CHECK)`, returns `KillOutcome::Requested`.
- Checkpoint in `trap_handler` (after the scause match, gated `SPP == 0`): if the gate
  `swap(false)` is set, `sched::exit_if_kill_requested()` → `note_exit(KILLED)` + wake
  parent + `exit_now_owned` (never returns). The task dies on its own hart.
- Race close: `prepare_switch` re-arms the gate when it runs a kill-flagged task, so a
  target that descheduled between flag-set and IPI still dies at its next return.
- `handle_kill` treats `Requested` like `Killed` (spend the cap + `CapEvent::Revoked`).

### Step 3 — cross-hart Kill (original notes)

Now reachable: a supervisor on hart 1 kills a child running on hart 0. Async by nature
(the killer can't touch a task live on another core):

- Add `Task.kill_requested: AtomicBool`.
- `kill_task`, for a `running_remote` target: set `target.kill_requested` (Release) +
  IPI the owning hart to make it trap promptly. Returns a new `KillOutcome::Requested`.
- At the proven-safe return-to-user checkpoint (right after `maybe_preempt` in
  `handle_timer`, gated on `from_user`): if the current task's `kill_requested` is set
  (Acquire), `note_exit(me, KILLED_STATUS)` + wake parent + `exit_now_owned()`. The
  task dies *on its own hart* — the only safe place to free its live stack/`satp`.
- `handle_kill` treats `Requested` like `Killed` (spend the cap + `CapEvent::Revoked`);
  the supervisor reaps via `WaitAny` once the target's hart processes the flag.
- Eventual-correctness note: if the target deschedules before the IPI lands, it dies on
  its next scheduled timer tick (the flag is the truth; the IPI is just a nudge). A
  target that blocks forever without being woken is the residual edge (rare; a blocked
  target is normally killed synchronously via the v2a/inc-3.5 path anyway).

### Step 4 — itest ✅

`workload=xhart-kill`: `xhart-killer` (hart 1) `SpawnOn`s a `hart-spinner` victim to
hart 0. Scenario `cross-hart-kill-stops-a-child` asserts `hart_spinner.up` `SpanStart`
with `hart_id == 0` (genuine cross-hart, not co-located), then `CapEvent::Revoked{Process}`
(cap spent), then `xhart.reaped` (self-terminated on hart 0 + reaped). **Passes in QEMU**
(max wait 0.7s). No regressions: SMP producer/consumer + supervision v2a still green.

**v2b complete.** All four steps ship and pass. The kill matrix has no deferred rows
left. Note (probabilistic): the victim tight-loops so it's *running* on hart 0 at the
kill with high probability (→ the `running_remote` async path); the rare moment it's
mid-preempt takes the synchronous off-CPU path — both correct, same observable outcome.

## Deferred / risks

- Hart-0 U-mode codegen hazards (the release-build `tp`-truncation class) — watch for
  them under `--release` itests once step 1 works.
- Whether hart 0 running userspace disrupts heartbeat cadence (the heartbeat yields, so
  a userspace hog on hart 0 is preempted by the quantum — but worth an eye on tick
  jitter in the metrics).
