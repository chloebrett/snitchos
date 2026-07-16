# Supervision v2 — plan

v1 (steps 0–4 + FU1/FU2) knows how to bring a service **back**. v2 is the other
direction: stop a service **deliberately** — for graceful shutdown and to restart a
*hung* (alive-but-wedged) service. Split into two milestones so the easy primitive
lands before the hard one.

- **v2a — `Kill` + graceful reverse-dep shutdown.** Needs one new primitive (`Kill`),
  no timer work. This doc.
- **v2b — timed-`WaitAny` + hung detection.** Needs a timer-driven timeout queue
  (wake a blocked waiter at a deadline). Deferred; sketched at the bottom.

## Decisions (2026-07-15)

- **`Kill` is hard.** The kernel primitive terminates + reaps the target task (the
  existing `Exit`/reap teardown path, triggered by another process). *Graceful* is
  built **in userspace**: the supervisor `Signal`s a shutdown [`Notification`] the
  service opted into, `WaitAny`s for a clean `Exit`, and hard-`Kill`s only as a
  fallback. No new signal mechanism — reuse the v0.12 `Notification` primitive.
  (The deadline that makes the fallback time-bounded is v2b; v2a's graceful path
  assumes cooperative services that do exit on the shutdown signal, with `Kill` as
  the force-stop for the rest.)
- **`Kill` is capability-authorized** by a new **`Object::Process { id }`** cap
  carrying a **`KILL`** right, minted at `Spawn` and delivered to the parent — the
  "lifecycle cap." It **composes** (a sub-supervisor can be granted `KILL` over its
  subtree) and matches the ambient-diet policy ("new syscalls default cap-gated").
  Not a bare parentage check (which is ambient + non-delegable).

## Increments (TDD each; policy-before-mechanism, mirroring v1)

### Status (2026-07-15)

- **Increment 1 ✅** — `teardown_order` in the `supervision` crate (17 tests green).
- **Increment 2 ✅** — the cap primitive, host-tested: `abi` (`Syscall::Kill = 30`,
  `rights::KILL = 0b100_0000`, `object_kind::PROCESS = 5`), `kernel_core::cap`
  (`Object::Process { id }`, `Rights::KILL`, `invoke_kill` + 3 tests, CapDesc arm),
  `protocol::CapObject::Process`. abi/protocol/kernel-core tests all green.
- **Increment 3 ✅ (mechanism; itest = inc 5)** — the `Ready`-only first cut, per §3a.
  - Pure policy `kernel_core::sched::classify_kill` + `KillAction` (6 host tests: Ready
    → Dequeue, Exited → NoOp, self/running-remote/blocked → refuse, precedence). 499
    kernel-core tests green.
  - `sched::kill_task(target) -> KillOutcome` + `KILLED_STATUS = -9` (a *convention*,
    forgeable by a self-`Exit`; the unforgeable kill signal is the `CapEvent::Revoked`).
    Reads placement (`CURRENT_TASK[h]` per hart via new `PerCpu::cells()`, runqueue
    scan, `state`), zombifies a `Ready` target in place, wakes its `WaitAny` parent.
  - `Spawn`/`SpawnImage` mint an `Object::Process { child }` (`KILL`) into the caller
    and return its handle in **`a1`** (`a0` unchanged) — `CapEvent::Granted`, cap-id
    spine wired.
  - `handle_kill`: `invoke_kill` → `kill_task`; on success **consumes** the lifecycle
    cap + emits `CapEvent::Revoked` (creation mints, destruction spends). Deferred
    targets refuse (reused `CapWrongObject`) + `emit_log` the precise reason.
  - Kernel builds for riscv64 with and without `itest-workloads`. **Not yet exercised
    in QEMU** — that's inc 5. (`cargo xtask` is currently broken by an unrelated
    working-tree `snemu/src/fwcfg.rs` WIP making snemu's `Machine` non-`Send`; the
    itest harness can't run until that's resolved.)

### 1. Pure policy — `teardown_order` (`supervision` crate, host-tested) ✅

`teardown_order(specs) = startup_order(specs).reverse()` — stop dependents before the
things they depend on. Same `Cycle` error. Trivial, but TDD it (it's the spine of
graceful shutdown). Host tests: linear chain reverses; a diamond stops `d` before
`b`/`c` before `a`; a cycle errors identically to `startup_order`.

### 2. ABI + cap primitive (`snitchos-abi`, `kernel-core::cap`, host-tested)

- `snitchos_abi::object_kind::PROCESS = 5`; `snitchos_abi::rights::KILL` (next free
  bit); `Syscall::Kill` (next free number).
- `kernel_core::cap::Object::Process { id: TaskId }` + `Rights::KILL`.
- `invoke_kill(table, handle) -> Result<TaskId, Denied>` — resolve `handle`, require
  `KILL` over an `Object::Process`, return the target `TaskId`. Mirrors `invoke_recv`.
  Host tests: accepts a `Process` cap with `KILL`; refuses one lacking `KILL`; refuses
  a non-`Process` object. Update `OwnedFrame`/`CapDesc` name mapping for the new kind.

### 3. `Kill` mechanism (`kernel/`)

- **`Spawn` mints the Process cap.** On a successful `Spawn`, insert an
  `Object::Process { id: child }` cap (rights `KILL`) into the **caller's** `CapTable`
  and return its handle in **`a1`** alongside the task id in `a0` (additive: existing
  `Spawn` callers read `a0` unchanged; `Kill`-users also read `a1`). Wire the cap-id
  spine (`parent_cap_id`) like other grant sites.
- **`handle_kill`** (syscall dispatch): `invoke_kill` → terminate + reap the target
  via the existing `Exit`/reap teardown (reclaim address space, wake any `WaitAny`
  parent with a killed-outcome status). Refuse (snitch `SyscallRefused`) without the
  cap. Emit a `CapEvent`/span for the kill.
- Covered by the QEMU itest (kernel mechanism isn't host-buildable).

#### 3a. `kill_task` — the task-state matrix (design before code)

`exit_now*` only terminates the *current* task. `kill_task(target)` terminates a task
that is **not** the one running. Where a target's id lives — and whether reaping it now
is safe — depends entirely on its scheduler state. Two facts drive the whole design:

1. **The `state` field does not track `Running`.** It's set to `Running` only at boot
   registration (`main`, hart-1 main); `prepare_switch`'s `Requeue` returns `None` from
   `next_state()`, so an *incoming* task is never re-labelled. A task that has yielded
   once keeps its stale label. ⇒ "is it running?" = `∃ hart: CURRENT_TASK[hart] ==
   target`; "is it ready?" = membership in some `runqueues[h]`. **Not** the `state` field.
2. **A `Blocked` task's id is parked in exactly one wait structure, but the `Task` table
   doesn't record which.** Reaping is safe for some structures and unsafe for others, so
   without a per-task *wait-kind* tag we can't tell a safe block from an unsafe one.

| Target state | Where the id lives | Reap-safe now? | Extraction procedure | v2a |
|---|---|---|---|---|
| **Exited** (zombie) | REAP status + table entry | yes | none — already dead; idempotent no-op = success | ✅ |
| **Ready** | a `Candidate` in some `runqueues[h]` | **yes** — not running ⇒ its `satp` isn't loaded, its stack is quiescent | `runqueues[h].remove(target)`; `state = Exited`; `note_exit(target, KILLED)`; `wake(parent)` | ✅ |
| **Running — self** | `CURRENT_TASK[me]` | n/a | that's `Exit`, not `Kill` | ❌ refuse (`BadArgument`) |
| **Running — other hart** | `CURRENT_TASK[other]` | **no** — live stack + loaded `satp` | IPI the hart → target reads a per-hart die-flag → self-`exit_now` | ❌ **defer v2b** |
| **Blocked — REAP / NOTIFY** | waiter slot in `ReapTable` / `NotifyTable` | yes — a stale waiter later `wake`s to a no-op (`on_wake(Exited)==false`) | `state = Exited`; `note_exit`; `wake(parent)`; leave the stale waiter | ⚠️ needs wait-kind tag |
| **Blocked — IPC endpoint** | id inside an endpoint's `SendersWaiting`/`ReceiversWaiting` queue | **no** — *semantically*: a future rendezvous pops the dead id, stashes a message under it, `wake`s a no-op → the sender's message vanishes into a ghost (inline msgs aren't copied into peer AS at rendezvous, so it's not a hard UAF, but it corrupts the endpoint) | must `remove` the id from every endpoint queue *before* zombifying | ❌ **defer v2b** |

**v2a scope (first cut).** `kill_task` handles **Ready** (the happy path) and **Exited**
(idempotent). It **refuses cleanly** — never crashes — for self, cross-hart-running, and
(pending the decision below) blocked targets. The `supervised-kill-stops-a-child` itest
kills a **Ready spinner** (a child that busy-yields, so it's always on a runqueue). The
reverse-dep-shutdown itest uses cooperative children that exit on a `Signal`ed shutdown
notification (`Signal` wakes a `WaitNotify`-blocked child → `Ready` → it exits itself), so
graceful shutdown never needs to kill a blocked task — `Kill` is only the Ready-spinner
force-stop. That keeps v2a's dangerous surface to the one provably-safe transition.

**Killed status.** A killed task's `WaitAny` parent should be able to tell a kill from a
clean exit. `kill_task` records `note_exit(target, KILLED_STATUS)`; the parent reads it in
`a0`. `KILLED_STATUS` = a documented kernel-side sentinel (`-9`, echoing SIGKILL though we
have no signals).

**Decisions (2026-07-17).** (Q1) **Defer all Blocked** — `kill_task` handles only
`Ready` + idempotent `Exited`; every `Blocked` target is refused (deferred to v2b), so
no new `Task` field and exactly one provably-safe transition ships. (Q2) **Reuse an
existing `RefusalReason` + `emit_log`** for the deferred/self cases — no new wire enum
variant (avoids the exhaustive-match ripple); the itest happy path kills a `Ready`
spinner and never hits it.

**Shape.** Pure classifier in `kernel_core::sched` (host-tested):
`classify_kill(is_self, running_remote, ready, state) -> KillAction` where `KillAction ∈
{ NoOp, Dequeue, RefuseSelf, RefuseRunningRemote, RefuseBlocked }`, precedence self →
running-remote → Exited(NoOp) → Ready(Dequeue) → Blocked. Kernel-side
`sched::kill_task(target)` reads the placement (`CURRENT_TASK[h]` per hart for running,
runqueue scan for ready, `task.state`), calls the classifier, and on `Dequeue` removes
the target from its runqueue, sets `state = Exited`, then (lock dropped)
`note_exit(target, KILLED_STATUS)` + `wake(parent)`. It does **not** reap inline — the
parent's `WaitAny` reaps the zombie via the existing `reap_task`. A killed task's open
spans are abandoned (no `SpanEnd`); its cursor goes inert on reap.

### 4. Graceful shutdown (userspace supervisor engine)

On a shutdown trigger, walk `teardown_order`: for each service, either `Signal` its
shutdown `Notification` and `WaitAny` its clean exit (cooperative), or `Kill` it
(force) — reaping each before moving to the next. Each stop is a span/`CapEvent` on
the wire. The reverse-dep order is the observable payoff (services stop in the mirror
of how they started).

### 5. Acceptance itest(s)

- **`supervised-kill-stops-a-child`** — the supervisor `Kill`s a running service and
  it's reaped (proves the primitive + the Process cap authorized it).
- **`supervised-shuts-down-in-reverse-dep-order`** — bring up a dep chain `a→b→c`,
  trigger shutdown, assert the stops land in reverse-dep order on the wire.
- **negative** — a process without the Process cap that tries to `Kill` is refused
  (a `SyscallRefused`), proving the authorization is real.

## The observability payoff

Graceful shutdown is a trace mirror-image of startup: services stopping in reverse
dependency order, each `Kill` an attributed `CapEvent`. The devlog money-shot for
v2a is "watch the tree come down in the exact reverse of how it came up."

## v2b (deferred) — timed-`WaitAny` + hung detection

- Extend `WaitAny` with an absolute-tick `deadline` (0 = block forever, backward
  compatible); return a `TimedOut` sentinel. Kernel: a **timeout queue** (`wake task
  T at tick D`) checked in the timer IRQ — the one genuinely new kernel bit.
- Hung detection: a service `Signal`s a liveness heartbeat the supervisor holds the
  `WAIT` end of; the supervisor `WaitNotify(deadline)`s; a timeout ⇒ the service is
  wedged ⇒ `Kill` + restart (reusing v2a's `Kill`). This is also what makes v2a's
  cooperative-shutdown fallback time-bounded.
