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
- **Increment 3 ⏳ WIP** — `Kill` is *wired* end to end (dispatch → `handle_kill`) and
  the kernel compiles, but `handle_kill` is an **inert stub that refuses**: the real
  teardown needs a `sched::kill_task(target)` primitive to stop a *non-current,
  possibly-blocked* task and wake its `WaitAny` parent — new scheduler machinery that
  must be QEMU-tested, so it wasn't rushed. `Spawn` does **not** yet mint the Process
  cap. This is the next, substantial piece. See `handle_kill`'s doc comment.

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
