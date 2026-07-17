# Timed WaitNotify/WaitAny + hung detection (v2b, part 2)

Supervision can bring a service **back** (v1 restart) and take one **down** (v2a Kill,
v2b cross-hart). The gap: detecting a service that is **alive but wedged** — not
crashed (so `WaitAny` never returns), not cooperating (so a shutdown `Signal` is
ignored). You cannot notice "stuck" without a **deadline**. This milestone adds one.

## What it enables

A supervisor arms a bounded wait. Two shapes, one mechanism:

- **Timed `WaitNotify`** — the hung-detection primitive. A service `Signal`s a liveness
  notification each work cycle; the supervisor `WaitNotify(deadline)`s the WAIT end. A
  beat within the deadline = healthy; a **timeout** = no progress = wedged ⇒ `Kill` +
  restart. (Plain `WaitNotify` can't do this: a healthy long-running service also never
  makes the wait return, so only the *absence of a beat within a deadline* signals
  "stuck".)
- **Timed `WaitAny`** — the same machinery for "a child should have exited by now": wait
  for a child's exit with a deadline; a timeout ⇒ it's taking too long ⇒ escalate.

## The mechanism: a per-hart timeout queue

A blocked waiter is a task on a specific hart. Each hart's timer IRQ (every ~50 ms)
drains its own timeout queue and wakes any task whose deadline has passed; the task's
wait loop re-checks, sees no event + a passed deadline, and returns `TimedOut`.

- **Pure core** (`kernel_core`, host-tested): `TimeoutQueue` over `BTreeSet<(deadline,
  TaskId)>` — `insert(deadline, task)`, `remove(task)` (idempotent, for a normal wake),
  `drain_expired(now) -> Vec<TaskId>` (pop every entry with `deadline <= now`). Mirrors
  `ipc::on_cancel` / the `ReapTable` pattern: pure bookkeeping, no kernel state.
- **Kernel wiring**: `PerCpu<Mutex<TimeoutQueue>>` (per-hart — the waiter registers in
  *its* hart's queue, that hart's timer drains it, `wake` re-enqueues on that hart).
- **Timer check** (`handle_timer`): after reading `now`, drain this hart's expired ids
  under the queue lock, release it, then `wake` each (never hold the queue lock across
  `wake`, which takes `SCHEDULER` — lock order queue → SCHEDULER). The timer preemption
  is *why* this works even when a wedged service is hogging the hart: the quantum
  preempts the spinner, the timer drains the queue, the supervisor is woken.
- **Granularity**: one timer tick (~50 ms). No `stimecmp` arm-to-earliest needed for
  hung-detection timescales; a later refinement if sub-tick deadlines are ever wanted.

## Blocking-path changes (both waits)

Today: `loop { match wait() { Ready(x) => return x, Block => block_current() } }`. With a
deadline:

```
if deadline != 0 { timeout_register(me, deadline) }   // this hart's queue
loop {
    match wait(...) {
        Ready(x)  => { timeout_remove(me); return Ok(x) }
        Block     => {
            block_current();                            // woken by event OR timeout
            if deadline != 0 && clock_now() >= deadline {
                cancel_waiter(...);                     // deregister as notify/reap waiter
                timeout_remove(me);                     // idempotent
                return TimedOut;
            }
            // else: early/spurious wake — loop and re-check
        }
    }
}
```

- **`cancel_waiter`** is the new bit that mirrors inc-3.5's `ipc::cancel_wait`: a
  timed-out `WaitNotify` must remove itself as the notification's parked waiter (else
  the "one waiter per notification" slot stays occupied and a later real waiter is
  wrongly refused), and a timed-out `WaitAny` must remove itself as an any-waiter in the
  `ReapTable`. Pure ops: `NotifyTable::cancel_wait(id, me)`, `ReapTable::cancel_wait_any(me)`.
- **Ordering hazard**: a `Signal`/child-exit could race the timeout (both fire near the
  deadline). The re-check under the event tables is the tiebreak — if bits/zombie are
  present, take them (not a timeout); the stale timeout-queue entry is removed idempotently.

## ABI (decision below)

Recommended: **extend `WaitNotify`/`WaitAny`, keep them backward-compatible by updating
the wrappers** (not new syscalls). The extra input register is a `deadline` (absolute
`time` ticks, `0` = block forever = today's behaviour). The existing runtime wrappers
(`Notification::wait`, `wait_any`) pass `deadline = 0` explicitly, so no caller sees a
change and there's no garbage-register hazard. New wrappers add the deadline:

- `Notification::wait_timeout(deadline) -> Result<Option<u64>, Denied>` — `Ok(Some(bits))`
  signalled, `Ok(None)` timed out, `Err` refused.
- `wait_any_timeout(deadline) -> Option<(i32, u32)>` — `Some((status, child))` reaped,
  `None` timed out.

Return encoding: `a1` (unused as an input on these) becomes the deadline in; on return,
`a1 = 1` marks a timeout (`a0 = 0`), `a1 = 0` a normal result, `a0 = usize::MAX` still a
refusal. Userspace builds the deadline from `clock_now()` + a timeout it computes via
`clock_freq()` (both already exist).

*(Alternative: dedicated `WaitNotifyTimeout`/`WaitAnyTimeout` syscalls, matching the
`SpawnOn` precedent. Safer against non-wrapper callers, but two more syscall numbers and
duplicated handlers. Since the wrappers are the sole callers, extending is clean — but
this is the call to confirm.)*

## Hung-detection demo + itest

- **Service** (`hung-service`): `Signal`s a liveness notification N times (each beat an
  observable `svc.beat`), then **wedges** — a tight `loop {}` that stops beating (alive,
  making no progress). Delegated the SIGNAL end at spawn.
- **Supervisor** (`hung-supervisor`): holds the WAIT end + the service's `Process` cap.
  Loops `wait_timeout(now + budget)`: each `Some(bits)` is a healthy beat (emit
  `beats_seen`); the first `None` (timeout) ⇒ `hung_detected` ⇒ `kill(service)` + reap.
- **itest** `supervisor-detects-and-kills-a-hung-service`: assert ≥1 `svc.beat`, then
  `hung_detected == 1`, then `CapEvent::Revoked{Process}` + the service reaped — the
  wedge was noticed *because of the deadline* and force-stopped.

## Increment breakdown

1. **Pure `TimeoutQueue`** + `NotifyTable::cancel_wait` + `ReapTable::cancel_wait_any`
   (kernel-core, host-tested, TDD).
2. **Per-hart queue + timer drain** (`handle_timer` → `wake` expired). Kernel; covered
   by the itest (no host path).
3. **Timed `WaitNotify`** (ABI extend + wrapper update + `wait_timeout`) — the
   hung-detection primitive.
4. **Timed `WaitAny`** (same machinery; `wait_any_timeout`).
5. **Hung-detection demo** (`hung-service` + `hung-supervisor` + workload wiring).
6. **itest** `supervisor-detects-and-kills-a-hung-service`.

## Decisions to confirm

- **ABI**: extend `WaitNotify`/`WaitAny` (update wrappers) vs. new `*Timeout` syscalls.
- **Queue scope**: per-hart (recommended — locality, no cross-hart wake) vs. one global.
- **Demo wedge shape**: tight-loop (simplest) vs. block-forever (exercises killing a
  *notify-blocked* task). Both work with the existing kill paths.
- **Scope**: ship timed `WaitNotify` (hung detection) first; treat timed `WaitAny` as a
  fast-follow on the same queue, or do both together.
