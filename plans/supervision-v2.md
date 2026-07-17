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

### Status (2026-07-17)

**Cross-hart `Kill` has since landed** — see
[multi-hart-userspace.md](legacy/multi-hart-userspace.md) Step 3. The "only cross-hart
running remains deferred" caveat in increment 3.5 below is **no longer true**:
`kill_task` now handles a target running on another hart by setting
`Task.kill_requested` + sending an `IPI_KILL_CHECK`, and the target
self-terminates at its next checkpoint (`KillOutcome::Requested`). The
`KillAction::RefuseRunningRemote` classifier arm survives in `kernel-core`, but
the kernel translates it into that async request rather than a refusal.

**Remaining here: increments 4 (graceful shutdown engine) and 5 (acceptance
itest).** The `Kill` mechanism itself is done.

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
- **Increment 3.5 ✅ (mechanism) — killing blocked services**, per §3b.
  - Pure `kernel_core::ipc::on_cancel` + 6 host tests (collapse-to-Idle, FIFO-preserving
    extraction, idempotent no-op). Kernel `ipc::cancel_wait(target)` scans the endpoint
    table + clears `pending`/`REPLIES`. 503 kernel-core tests green.
  - Classifier collapsed: `KillTarget` drops `ready`; `KillAction::{Dequeue,
    RefuseBlocked}` → one `Terminate`. `kill_task` calls `cancel_wait` unconditionally on
    the terminate path. Only cross-hart *running* remains deferred (v2b, needs an IPI).
  - Documented safe limitation: a task killed mid-`Call` leaves a lingering one-shot
    reply cap (server's reply safely no-ops). Builds with/without `itest-workloads`;
    QEMU exercise still pending inc 5.

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
| **Blocked — REAP / NOTIFY** | waiter slot in `ReapTable` / `NotifyTable` | yes — a stale waiter later `wake`s to a no-op (`on_wake(Exited)==false`) | `state = Exited`; `note_exit`; `wake(parent)`; leave the stale waiter | ✅ **(inc 3.5)** — not in any endpoint queue, so `cancel_wait`'s scan no-ops and the zombify is safe as-is |
| **Blocked — IPC endpoint** | id inside an endpoint's `SendersWaiting`/`ReceiversWaiting` queue | **no** — *semantically*: a future rendezvous pops the dead id, stashes a message under it, `wake`s a no-op → the sender's message vanishes into a ghost (inline msgs aren't copied into peer AS at rendezvous, so it's not a hard UAF, but it corrupts the endpoint) | `cancel_wait` removes the id from every endpoint queue (`on_cancel`) + clears its `pending`/`REPLIES`, *then* zombify | ✅ **(inc 3.5)** |

**v2a scope (first cut — *superseded by inc 3.5, §3b*).** The initial cut handled only
**Ready** + **Exited** and refused blocked targets. Inc 3.5 lifted that: `kill_task` now
terminates any **off-CPU** target (`Ready` or `Blocked`), refusing cleanly only self and
cross-hart-running. Kept here as the design record; the live behaviour is §3b.

**Killed status.** A killed task's `WaitAny` parent should be able to tell a kill from a
clean exit. `kill_task` records `note_exit(target, KILLED_STATUS)`; the parent reads it in
`a0`. `KILLED_STATUS` = a documented kernel-side sentinel (`-9`, echoing SIGKILL though we
have no signals).

**Decisions (2026-07-17).** (Q1) Originally **defer all Blocked**; **superseded by inc
3.5** (§3b), which supports blocked targets via `cancel_wait` — the only remaining
deferral is cross-hart *running*. (Q2) **Reuse an existing `RefusalReason` + `emit_log`**
for the deferred/self cases — no new wire enum variant (avoids the exhaustive-match
ripple); the itest happy path terminates cleanly and never hits it.

#### 3b. `cancel_wait` — killing blocked services (inc 3.5)

The Ready-only cut can't kill the realistic uncooperative service: an idle IPC server
blocked in `Receive`. Inc 3.5 lifts that, and the key realisation makes it *shrink* the
code rather than grow it: **blocked ⇒ not running ⇒ its stack + `satp` are quiescent**,
so reaping a blocked target is always *memory*-safe. The only hazard is the dangling id
left in an endpoint wait queue (the ghost rendezvous). Every other block site
(`WaitNotify`, `Wait`/`WaitAny`, a `Call` already-delivered) parks the id somewhere
whose stale entry degrades to a harmless no-op `wake`, so it needs no cleanup at all.

So the whole fix is: extract the id from any endpoint queue, then zombify like a Ready
target.

- Pure `kernel_core::ipc::on_cancel(state, me) -> EndpointState` — remove `me` from the
  waiting queue, collapse to `Idle` if it was the last (mirror of `on_send`/`on_receive`,
  upholding the never-empty-queue invariant). Idempotent (a no-op if `me` isn't waiting).
  Host-tested (6 cases).
- Kernel `ipc::cancel_wait(target)` — scan every endpoint (the id is parked in at most
  one; the count is tiny, so a scan beats a per-task tag), apply `on_cancel`, drop any
  `pending` message the target stashed and any awaited `REPLIES` slot. Safe + no-op for a
  `Ready` target (in no queue), so `kill_task` calls it unconditionally.
- The classifier **collapses**: `KillAction::{Dequeue, RefuseBlocked}` → one `Terminate`
  covering `Ready`-or-`Blocked`; `ready` drops out of `KillTarget` entirely (it's
  mechanism, not policy). What stays deferred to v2b is only cross-hart *running* (needs
  an IPI) — which a same-hart supervisor demo never hits.

**Known limitation (documented, safe):** a task killed mid-`Call` (already delivered,
awaiting reply) leaves a one-shot reply cap in the server naming the dead task; the
server's eventual `reply` safely no-op-`wake`s it, and the cap lingers until the server
is reaped. No UAF, just a wasted reply. Reply-cap invalidation on kill is a later
refinement.

**Shape (final).** Pure classifier in `kernel_core::sched` (host-tested):
`classify_kill(KillTarget { is_self, running_remote, is_exited }) -> KillAction` where
`KillAction ∈ { NoOp, Terminate, RefuseSelf, RefuseRunningRemote }`, precedence self →
running-remote → Exited(NoOp) → Terminate. Kernel-side `sched::kill_task(target)` reads
the placement (`CURRENT_TASK[h]` per hart via `PerCpu::cells()` for running, `task.state`
for exited), calls the classifier, and on `Terminate` removes the target from any
runqueue + sets `state = Exited` (under the lock), then (lock dropped) `cancel_wait` +
`note_exit(target, KILLED_STATUS)` + `wake(parent)`. It does **not** reap inline — the
parent's `WaitAny` reaps the zombie via the existing `reap_task`. A killed task's open
spans are abandoned (no `SpanEnd`); its cursor goes inert on reap.

### 4. Graceful shutdown (userspace supervisor engine)

On a shutdown trigger, walk `teardown_order`: for each service, either `Signal` its
shutdown `Notification` and `WaitAny` its clean exit (cooperative), or `Kill` it
(force) — reaping each before moving to the next. Each stop is a span/`CapEvent` on
the wire. The reverse-dep order is the observable payoff (services stop in the mirror
of how they started).

#### 4a. Design (2026-07-17) — a new `supervised-shutdown` workload

Kept **separate** from the crash-restart `supervised` engine (whose itests assert its
crash-loop behaviour) — a new root bin + `WorkloadKind::SupervisedShutdown`. Shape:

- **Userspace runtime (foundational):**
  - `kill(process_cap: u32) -> Result<(), Denied>` — wraps `Syscall::Kill` (a0 = the
    `Object::Process` handle; `usize::MAX` = refused, mirroring `signal`/`wait`).
  - `spawn_supervised(program, handles) -> Option<Child>` where `Child { task, kill }`.
    Captures the inc-3 Process-cap handle the kernel writes back in `a1` (via
    `inlateout("a1")` — the kernel reads `a1` as the handles ptr, then overwrites it
    with the handle). `spawn` stays as-is for the ~11 existing callers.
- **Cooperative worker** (`user/hello/src/bin/svc-worker.rs`, new SPAWNABLE id): reads a
  delegated shutdown `Notification` at `delegated_handle(0)`, emits
  `snitchos.svc.<name>.up = 1` (liveness), then `WaitNotify`s — on signal, `exit(0)`.
- **Forced service**: reuse `spinner` (program 3) — never cooperates, so the supervisor
  must `Kill` it (proving the inc-3 primitive + the Process cap authorized it).
- **Supervisor** (`user/hello/src/bin/supervised-shutdown.rs`): service table with a
  dep chain + a per-service `Shutdown { Cooperative(notify) | Forced }`. Bring up in
  `startup_order` (`notify_create` + delegate for cooperative ones), each proving `up`.
  Then walk `teardown_order`: cooperative → `Signal` its notify + `wait_any` its clean
  exit; forced → `kill(child.kill)` + `wait_any` its reap. Emit
  `snitchos.svc.<name>.stopped` per stop — the **emission order is the reverse-dep
  proof**. A forced stop also puts a `CapEvent::Revoked{Process}` on the wire (the
  kernel spends the lifecycle cap).

Not host-testable (no_std syscall bins); validated by inc 5. Only compilation gates it
until the `cargo xtask` breakage is resolved.

**Increment 4 ✅ (mechanism; itest = inc 5).** Shipped:
- Runtime (`snitchos-user`): `kill(process_cap) -> Result<(), Denied>`; `Child { task,
  kill }` + `spawn_supervised(program, handles) -> Option<Child>` (captures the inc-3
  Process-cap handle from `a1` via `inlateout`). `spawn` unchanged for existing callers.
- `user/hello/src/bin/svc-worker.rs` (cooperative service, SPAWNABLE id 10): emits
  `snitchos.svcworker.up`, `WaitNotify`s a delegated shutdown notification, `exit(0)`.
- `user/hello/src/bin/supervised-shutdown.rs` (root): `alpha→beta→gamma` tree (two
  cooperative workers + a forced `spinner`); brings up in `startup_order` (delegating a
  per-service notification to cooperative ones), tears down in `teardown_order` —
  `Signal` for cooperative, `kill` for forced — reaping each and emitting
  `snitchos.svc.<name>.stopped` in reverse-dep order.
- Wiring: `WorkloadKind::SupervisedShutdown` + parse arm (host test); kernel ELF
  statics, SPAWNABLE id 10, `SUPERVISED_SHUTDOWN` ProgramSpec, LAYOUTS entry, main.rs
  dispatch arm; `build.rs` rows for both bins.
- Kernel builds (default + `itest-workloads`) with both new bins embedded; 514
  kernel-core tests green. **Not yet QEMU-exercised** — inc 5, still blocked on the
  `cargo xtask`/snemu-WIP breakage.

### 5. Acceptance itest(s)

- **`supervised-shuts-down-in-reverse-dep-order`** ✅ — brings up `alpha→beta→gamma`,
  asserts `gamma.stopped → beta.stopped → alpha.stopped → complete` in cursor order
  (reverse-dep). **Passes in QEMU** (max wait 0.2s).
- **`supervised-kill-stops-a-child`** ✅ — asserts the forced `spinner`'s
  `CapEvent::Revoked{Process}` (unforgeable kill proof + cap authorization) then
  `gamma.stopped`. **Passes in QEMU.**
- **`kill-without-a-process-cap-is-refused`** ✅ — `workload=kill-no-cap`: a lone
  process holding no `Process` cap tries `kill(99)`; asserts `SyscallRefused{Kill,
  CapNotFound}` (unforgeable, kernel-emitted) then `killnocap.refused == 1` (it
  survived and observed the refusal). Proves the authorization is real, not ambient.
  **Passes in QEMU.**

**v2a is complete.** All three acceptance scenarios pass end-to-end. The `cargo xtask`
blocker is resolved (snemu `Machine: Send` fixed; the tree also gained a `kernel-mem`
crate extraction). Deferred to v2b: cross-hart-running `Kill` (needs an IPI), timed
`WaitAny` + hung detection.

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
