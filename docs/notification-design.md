# 🔔 Notification design

*The async kernel→user signal, stripped to the bone. Mechanism in the kernel, meaning in userspace. Capability-gated. Every signal traced.*

Scoped for **v0.12**. Child-exit is the first consumer (a parent `Wait`ing on a child); the reason it earns its own object — rather than staying the bespoke reap path it is today — is that **device interrupts reuse the exact same path**: "wake userspace when something happened" reaps a zombie now and delivers a keystroke later. Build it once, generically, where the kernel never learns what the something *means*.

This page commits the object shape and the syscall surface before any code, the same way [ipc-design.md](ipc-design.md) did for endpoints. The IPC doc already named this primitive ("Notifications — the async primitive … a per-object set of bits … carries no payload"); this is that sketch made concrete.

---

# Why a new primitive at all

We already wake a parent when a child exits — `ReapTable` + the kernel's `block_current`/`wake`. So why not just keep doing that per case?

Because that path is **welded to one meaning.** `kernel_core::reap::ReapTable` knows about zombies and exit statuses; `sched::wait_for`/`note_exit` know about parents and children. The next async wake — a device IRQ arriving for a driver — has nothing to do with zombies, but it needs the identical control-flow: *a userspace task parks; something happens in the kernel (or another task); the task is made runnable and told it happened.* Two bespoke copies of that is two places to get the blocking/wake race wrong.

A `Notification` is that control-flow **with the meaning removed.** The kernel carries a bit; userspace decides the bit means "your child exited" or "the UART has a byte" or "the timer fired." Same object, same syscalls, same trace shape — interpreted entirely above the kernel. This is the project's recurring move (badges, span names, metric names): *the kernel provides the mechanism and stays ignorant of the semantics.*

The lineage is seL4's `Notification` object — an async, coalescing signal with no payload. We adopt its semantics deliberately; the differences (below) are about SnitchOS's observability pillar, not the core idea.

---

# The object: a coalescing signal word

A `Notification` is **one machine word of pending-signal bits** plus at most one parked waiter.

- **Signal** sets bits: `pending |= mask`. Never blocks, never fails for a valid cap. If a waiter is parked, wake it.
- **Wait** consumes bits: if `pending != 0`, return it and clear to 0 (read-and-clear); else park the caller and block. On wake, return the bits that were set and clear.

Two properties fall straight out, and both matter:

1. **Coalescing.** Three signals before anyone waits collapse into one wake carrying the OR of the bits. There is **no queue, no buffering, no per-signal kernel memory** — exactly the property that made synchronous endpoints attractive (no IPC-driven resource exhaustion). A notification cannot be made to grow kernel memory by spamming it. This is the deliberate difference from a message: a notification answers *"did it happen?"*, never *"how many times, in what order, with what data."* If you need that, you need an endpoint.

2. **Edge, not count.** Because bits OR together, a notification is level/edge-ish, not a counter. A driver that wants "N interrupts arrived" counts them itself in userspace after each wake; the kernel guarantees *at least one* wake per signal-after-drain, not one-per-signal. (This is precisely how real IRQ handlers behave — you service all pending work on each wake, because coalescing is always possible.)

The bit *mask* is the one word of meaning we permit, and it is **userspace-defined**, like a badge. The kernel never reads it beyond OR-ing and delivering it. A process that holds one notification for several event sources assigns each source a bit and demuxes on wake; a process that wants one notification per source ignores the mask entirely (signals with bit 0).

## Pure core, mirroring `ReapTable`

The bookkeeping is pure data — no `unsafe`, no MMIO, host-tested — exactly like `kernel_core::reap` and `kernel_core::ipc`. Sketch:

```rust
// kernel_core::notify
pub enum SignalStep { Woke(TaskId), NoWaiter }   // kernel wakes the returned task
pub enum WaitStep   { Ready(u64), Block }         // bits, or park-and-block

pub struct Notification { pending: u64, waiter: Option<TaskId> }

impl Notification {
    fn signal(&mut self, mask: u64) -> SignalStep;  // pending |= mask; wake waiter if any
    fn wait(&mut self, caller: TaskId) -> WaitStep; // pending!=0 → Ready(take); else park
}
```

The shape is intentionally the `on_wait`/`on_exit` mirror of `ReapTable`: `wait` either returns immediately or records-and-blocks; `signal` returns the parent to wake (if any) and the kernel runs `wake`. The kernel side owns the live table behind a `Mutex` and does the `block_current`/`wake` wiring — the core never touches a register or a CSR.

**One waiter per notification in v0.12.** A second `Wait` while one is parked is *refused* (snitched), not silently dropped — we learned that lesson from `ReapTable.waiters`' single-slot overwrite (v0.12 edge #3). Multi-waiter fan-out (one signal wakes N) is a documented growth point, not v0.12 scope.

---

# Capability shape

A new `Object` variant, named by `NotificationId`, gated by two disjoint rights — the same two-ended split as endpoints' `SEND`/`RECV`:

```rust
Object::Notification { id: NotificationId }

Rights::SIGNAL   // may Signal this notification (the producer end)
Rights::WAIT     // may Wait on this notification (the consumer end)
```

- Disjoint bits so one cap grants the producer end, another the consumer end, or one cap both. A driver holds `WAIT`; the kernel's IRQ path (or a peer task) holds `SIGNAL`.
- New `rights::SIGNAL` / `rights::WAIT` bits in `snitchos_abi::rights` (the single source of truth shared with userspace) — next free bits past `MINT = 0b1000`.
- `Multiplicity::Persistent` — a notification is signalled and waited repeatedly, like an endpoint, unlike the one-shot reply cap.
- No badge in v0.12. The signal *mask* already carries userspace-defined demux meaning; a badge (server-stamped, kernel-opaque) is the endpoint mechanism and can be added later by the same precedent if a single notification needs to distinguish *who* signalled. Not needed for child-exit or single-IRQ-per-driver.

Creation: a `NotifyCreate` syscall mints a fresh `Notification { id }` cap with `SIGNAL | WAIT` into the caller's table (the caller then attenuates + delegates the ends it wants, e.g. hands a child a `SIGNAL`-only cap). This mirrors how a process gets its first endpoint. The kernel allocates the `NotificationId` and the table slot; the holder delegates via the existing cap-transfer machinery.

---

# Syscall surface

Three new numbers appended past `Wait = 18` (next free are 19–21). Syscall numbers are **not frozen** — kernel and all userspace compile from one build (the user ELFs embed into the kernel image), so appending is free; only the postcard *frame* format is frozen. (Established in post 33 when `Invoke` was renumbered out.)

| `a7` | name | args | returns | rights |
|---|---|---|---|---|
| 19 | `NotifyCreate` | — | `a0` = handle to a fresh `SIGNAL\|WAIT` cap | (ambient: making your own notification) |
| 20 | `Signal` | `a0` = handle, `a1` = bit mask | `a0` = 0 / refused | `SIGNAL` |
| 21 | `WaitNotify` | `a0` = handle | `a0` = bits that were pending (read-and-cleared) | `WAIT` |

- `Signal` resolves the cap, checks `SIGNAL`, OR-s the mask, wakes any parked waiter, returns. Never blocks.
- `WaitNotify` resolves the cap, checks `WAIT`; nonzero pending → return-and-clear; else `block_current()`, and on wake return the (now-cleared) bits.
- Refusals snitch (`SyscallRefused` + counter), never silent — the project invariant. Refusal cases: bad handle, missing right, second waiter on an already-waited notification.

Polling (`WaitNotify` that returns 0 instead of blocking when empty) is a possible variant — useful for an event loop that interleaves several sources — but the blocking form is the primitive; a non-blocking `poll` flag in `a1` is an additive follow-on if a consumer needs it.

---

# Child-exit, re-expressed (the migration question)

This is the design decision worth deciding explicitly, because there are two honest options.

**Today:** `Exit` → `note_exit` records a zombie + returns the parent → `wake`. `Wait` → `wait_for` reaps-or-blocks. The *status* (an `i32`) lives in `ReapTable`; the *wake* is bespoke.

The notification primitive cleanly absorbs the **wake**, but not the **status** — a notification carries bits, not an `i32` exit code. So:

- **Option A — leave `Wait`/`Exit` as-is; notifications are a parallel primitive.** v0.12's reap path already works, is host-tested + mutation-clean, and ships. Notifications are built alongside for the *general* case (devices, future signals), proven by their own small consumer. `Wait` keeps its bespoke `ReapTable` because it needs to convey a status the notification word can't.
  - *Pro:* doesn't reopen a shipped, verified path; smallest diff; the two stay legible (`Wait` is "reap a child + get its code," `WaitNotify` is "wait for an anonymous event").
  - *Con:* two wake mechanisms coexist; the "build it once" framing is aspirational, not literal, for v0.12.

- **Option B — child-exit *is* a notification.** Each process gets a bound exit-notification; `Exit` signals it; the parent's `Wait` becomes `WaitNotify` + a separate "collect status" lookup. (This is seL4's bound-notification shape.)
  - *Pro:* one wake mechanism, literally; the strongest version of the thesis.
  - *Con:* reopens a shipped path; splits "wait" from "get status" into two steps or bolts a status side-channel onto the notification; more risk for no new capability.

**Decided: A for v0.12, B as the consolidation it points at.** Ship the notification object proven by a *fresh* minimal consumer (two tasks, one signals, one waits — observable on the wire), and keep the reap path that already works. The honest framing — same as post 33's name-GC deferral — is that the *general* primitive and the *first specialised* path can coexist for one milestone; folding child-exit onto it is a clean follow-on once the device IRQ consumer (the second real user) proves the generic shape under real async load. Building the generic object now is what makes that fold cheap later; doing the fold now buys nothing v0.12 needs.

The device-IRQ consumer is the one that *forces* the primitive to be right, because an IRQ genuinely is async and payload-free — there is no status to convey, just "it happened." That's the consumer the design is really for; child-exit is the one we have first.

---

# Observability — the post angle

The reason any of this is interesting here: **an async wake is exactly the causality a synchronous trace cannot show.** A synchronous `call` is a cross-process call stack — the span structure reads off the control flow. A notification breaks that on purpose: the signaller and the waiter are *not* in a call/return relationship; the wake arrives out of band. So the trace has to stitch it back.

Two new frames (appended — never reorder):

- `NotifySignal { notification, mask, from_task }` — emitted when a signal lands.
- `NotifyWait   { notification, bits, to_task }` — emitted when a waiter wakes (or returns pending immediately).

In Tempo these let you *see the edge*: task X signals at t₀, task Y was parked, Y wakes at t₁ carrying the same notification id — a dependency arrow that isn't a call stack. For the device case it's the headline: an IRQ frame, then the driver task waking on the bound notification, then the driver's handling span — *"watch an interrupt become a userspace wake."* The snitch narrates the one control-transfer that's normally invisible.

(Frame budget note: `NotifySignal` on a hot IRQ source could flood the wire. The mitigation is the same coalescing the object already does — emit on the *signal that actually transitions empty→nonempty* and on each wake, not on every redundant OR into an already-set bit. Decide at implementation; flag if a high-rate source makes it matter.)

---

# What v0.12 ships vs. defers

**Ships:** the `Notification` object (`kernel_core::notify`, host-tested), `Object::Notification` + `SIGNAL`/`WAIT` rights, `NotifyCreate`/`Signal`/`WaitNotify` syscalls + runtime bindings, one waiter per notification (second waiter refused + snitched), the two wire frames, and a minimal two-task itest scenario (`A` signals, `B` waits, assert `B` wakes with the right bits and a `NotifySignal`→`NotifyWait` pair on the wire).

**Defers (documented growth points, not silent gaps):**
- **Folding child-exit onto the notification** (Option B) — after the device-IRQ consumer validates the generic shape.
- **Multi-waiter fan-out** — one signal wakes N waiters. Needs a waiter list, not a slot.
- **Badged notifications** — distinguishing *who* signalled, by the endpoint-badge precedent. The mask covers single-process demux without it.
- **Non-blocking `poll`** — a flag on `WaitNotify` for event loops over several sources.
- **IRQ→notification binding** — the kernel trap handler signalling a notification a driver waits on. This is the *next consumer* and the real point of the primitive; it lands when device drivers do (post-v1.0 arcade/HAL arc), reusing this object unchanged.

---

# References

- [docs/ipc-design.md](ipc-design.md) — the synchronous endpoint half; notifications are its async sibling (the doc sketches both).
- `kernel_core::reap` (`kernel-core/src/reap.rs`) — the pure-core bookkeeping pattern this mirrors; also the path child-exit uses today.
- `kernel_core::user::cap` — `Object` / `Rights` / `Multiplicity`; where the `Notification` variant + `SIGNAL`/`WAIT` bits land.
- `kernel::sched::{block_current, wake}` (`kernel/src/sched/mod.rs`) — the park/unpark primitives the kernel side reuses verbatim.
- [plans/v0.12-exit-wait.md](../plans/v0.12-exit-wait.md) — edge #2 ("general notification primitive — TODO") is this document's mandate.
- [docs/roadmap-and-milestones.md](roadmap-and-milestones.md) — v0.12 lifecycle; v0.13 shell is the next consumer of a reliable `Wait`.
