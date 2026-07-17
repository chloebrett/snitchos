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

### Step 1 — prove a userspace process runs on hart 0 (de-risk) ⏳

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

### Step 2 — hart-pinned userspace Spawn

Let a userspace supervisor place a child on a chosen hart (today `Spawn` pins to the
caller's hart). Options (decide at step 2): a `Spawn` ABI variant carrying a target
hart, or a dedicated `SpawnOn`. The child lands on the other hart's runqueue + an
`IPI_WAKEUP` nudges it (as `spawn_on` already does cross-hart).

### Step 3 — cross-hart Kill

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

### Step 4 — itest

`workload` with a supervisor on hart 1 and a child on hart 0; the supervisor kills the
cross-hart child. Assert `CapEvent::Revoked{Process}` + the child reaped, and (ideally)
that the child was running on hart 0 (a frame tagged `hart_id == 0`) before the kill —
so the test genuinely exercises the cross-hart path, not a co-located one.

## Deferred / risks

- Hart-0 U-mode codegen hazards (the release-build `tp`-truncation class) — watch for
  them under `--release` itests once step 1 works.
- Whether hart 0 running userspace disrupts heartbeat cadence (the heartbeat yields, so
  a userspace hog on hart 0 is preempted by the quantum — but worth an eye on tick
  jitter in the metrics).
