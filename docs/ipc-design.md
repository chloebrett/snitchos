# 📬 IPC design

*Mechanism in the kernel, policy in userspace. Synchronous by default. Capability-gated. Every message traced.*

**Status: shipped.** Synchronous endpoints landed at **v0.9**, badged endpoints + cap-transfer at **v0.9c**, and notifications (the async primitive) at **v0.12**. This page is the design rationale; it predates some of that numbering, so trust the per-section ✅ markers and `docs/roadmap-and-milestones.md` for what's actually built.

> **Numbering note:** this page was written before SMP (v0.6) and preemption (v0.8) were inserted, which pushed everything downstream. The plan-era "IPC at v0.9" became: synchronous endpoints + `call`/`reply` at **v0.9/v0.9b**, badged endpoints at **v0.9c**, and the async **notification** primitive at **v0.12** (it was never a "v0.9d" — the roadmap went v0.9→v0.10 FS→v0.11 console/spawn→v0.12 exit/wait+notifications). userspace + capabilities are **v0.7a/v0.7b**.

# Stated philosophy: "don't communicate by sharing memory" — at the OS level
Go's slogan — *don't communicate by sharing memory; share memory by communicating* — is the guiding philosophy, but it reframes at the OS level.

Go's advice targets a *concurrency model*: inside one address space, "share memory" is the easy, dangerous default, and message-passing is the discipline imposed on top. In a microkernel the situation is **inverted** — separate processes do not share an address space, so isolation is the default and sharing must be deliberately constructed. Any shared memory in SnitchOS is a capability-gated `MemoryRegion`, mapped only into the processes that explicitly hold the cap.

The two ideas operate at different layers and agree:

- **Concurrency layer:** prefer message-passing and ownership transfer over lock-guarded shared state. SnitchOS's synchronous IPC *is* this — a `send` is an ownership handoff.
- **Mechanism layer:** "shared memory" is just a transport technique for moving bulk data. It is plumbing under the model, not the interface. Go isn't against memory being the transport; it's against memory being the *interface*.

The directive: the interface userspace sees is **channels and messages**, even when the transport underneath is a shared `MemoryRegion`. Same interface-vs-implementation principle as everywhere else in SnitchOS. (Caveat: SnitchOS cannot make data races *structurally* impossible the way Go's runtime + single language can — userspace is multi-language and WASM. The channel library makes the safe path idiomatic; it cannot make the unsafe path unrepresentable. A pit of success, not a wall.)

# Two primitives: synchronous endpoints + notifications

## Synchronous endpoints — the workhorse
A synchronous send is a **rendezvous**. Sender calls `send(endpoint, msg)`; if no receiver is waiting, the sender **blocks** (parked, off the run queue). When a receiver calls `receive`, the kernel copies the message directly sender→receiver and can do a **direct context switch** — run the receiver immediately, no scheduler round-trip. The message is never buffered; it goes straight across. Reply works the same way in reverse.

Why this is the default for SnitchOS specifically:

- **Tiny kernel.** No queues, no buffer memory to manage — serves the <10K-line goal.
- **Free backpressure and resource accounting.** A blocked sender is just a parked thread. No kernel memory grows anywhere. No IPC-based resource-exhaustion attack surface — serves the security pillar.
- **Clean spans.** A synchronous call *is* a cross-process call stack. The span opens when the caller calls, closes when the callee replies. Causality is read off the control flow, not stitched back from context IDs — serves the observability pillar. Async IPC would make the killer feature harder to build.
- **Reasoning.** When `send` returns, the message was received. No delivery ambiguity.

Accepted costs: deadlock is possible (A calls B, B calls A) — requires acyclic call-graph discipline and/or abortable/timeout sends. Servers must have a specific loop shape (loop on receive). Both are well-understood and both are good blog content.

## Notifications — the async primitive ✅ (v0.12)
The physical world is asynchronous — interrupts happen whether or not anyone is ready to receive. So an async primitive is mandatory. But it does not have to be buffered channels; it is a **notification**: async signalling stripped to the bone. A per-object word of bits; signalling sets bits and wakes any waiter; it carries no payload. No arbitrary-message buffer, so no buffering problem. Models interrupts arriving and readiness signals ("data is ready, come do a synchronous receive").

It is the **async dual of the synchronous endpoint**: a `send`/`call` *rendezvouses* (the initiator can block, a message crosses); a `signal` is *fire-and-forget* (the signaller never blocks, no data, signals **coalesce**). What shipped (plan: [plans/legacy/v0.12-notifications.md](../plans/legacy/v0.12-notifications.md); pure core `kernel-proc/src/notify.rs`):

- `Object::Notification { id }` with `SIGNAL` / `WAIT` rights (the producer/consumer split, like the endpoint's `SEND`/`RECV`). Created by `NotifyCreate` (returns a `SIGNAL | WAIT` cap); ends handed out via cap-transfer.
- `Signal(notif, mask)` ORs `mask` into the pending bits and wakes a waiter — **never blocks, never fails for resources**. Repeated signals before a wait coalesce (it's a bitset OR, not a queue).
- `WaitNotify(notif)` returns the pending bits **and clears them** (drain), or blocks until signalled. **One waiter** per notification (a second is refused, `NotificationBusy`) — multi-waiter wake is a deferred generalization.
- Observable: `NotifySignal` / `NotifyWait` wire frames.

> **Notifications are one leg of userspace device drivers** (the roadmap's v1.2 smoltcp-over-virtio-net goal). A driver needs three things: (1) **interrupts delivered as notifications** — the kernel's IRQ handler `signal`s, the driver thread `wait`s — *this primitive*; (2) **MMIO register access** granted as a device-memory capability; (3) **DMA buffers** the device addresses physically. Notification is necessary but not sufficient. (The virtio-*console* stays in-kernel regardless — it's the boot-time telemetry lifeline, so it can't depend on a userspace process that doesn't exist yet.)

# Message payload: inline words, bulk copy, shared regions
Three tiers, by size and synchrony:
- **Small messages** — copied inline through `MSG_WORDS` registers (`snitchos_abi::MSG_WORDS` = 4, L4-style). The fast common path; carries opcodes + scalars + `(ptr, len)` references.
- **Large synchronous payloads** — a **cross-address-space copy** authorized by the reply cap (option D; see *Bulk transfer* below). The synchronous RPC answer: the data is *moved* (message passing), nothing is shared. This is what the filesystem's `read`/`write` use.
- **Large async / streaming payloads** — a shared `MemoryRegion` + a `Notification`, behind the userspace channel library (see *Async-with-data*). Zero-copy for sustained throughput; the deliberate exception to "don't share memory," confined to the channel library.
- A message is therefore *some inline words + (a cross-AS copy | a shared region) + some capabilities*. "Passing data" and "passing a capability" are the same mechanism.

# Endpoint capabilities: badges, minting, and cap-transfer ✅ (v0.9c)

**Shipped in v0.9c.** A single endpoint serves many clients and many objects behind one receive loop. The filesystem is the motivating consumer (see [filesystem-design.md](filesystem-design.md) → *Capability mechanism*): one FS endpoint, one file capability per `(inode, rights)`. Plan + step-by-step rationale: [plans/legacy/v0.9c-badges.md](../plans/legacy/v0.9c-badges.md).

**The framing that made it small: a badge is the generalized reply cap.** v0.9b's `Object::Reply { caller }` was already a kernel-stamped, unforgeable value transferred into a process's table and delivered on invoke. v0.9c freed that mechanism along three axes already in the code — *who mints* (kernel→server), *lifetime* (`Once`→`Persistent`, the existing `Multiplicity`), and *stamped value* (`caller`→arbitrary `badge`). The general thing is *less* mechanism than the special case. And all three pieces below are generic because **the kernel never learns what the server's objects mean**.

## Badges — unforgeable per-cap labels
`Object::Endpoint { id, badge: u64 }` (`badge == 0` = the bare owner/`RECV` cap; nonzero = a derived `SEND` cap).
- **Server-chosen, set once at mint, immutable** thereafter. The holder of a `MINT`-righted endpoint cap picks each derived cap's badge; no one can re-badge or forge it. The kernel never reads it beyond carrying it.
- **Delivered to the receiver on every message** in register **`a6`** (`receive`/`receive_with_reply`). The kernel stamps it from the cap the *sender used*; the receiver demuxes — "which of my objects / which client is this?" — against its own table. `a6 = 0` for a bare cap.
- **The sender cannot influence it.** A client holding `badge(inode=7, READ)` cannot present any other badge; the authority is the cap, not a number in the message.

One endpoint thus stands in for an unbounded set of server objects with no kernel object per object — the badge *is* the object selector, interpreted entirely in userspace.

## Mint / derive
The `MintBadged` syscall (`a0` = endpoint handle, needs `MINT`; `a1` = badge; `a2` = rights) derives a child `Endpoint` cap naming the same endpoint, stamped with the badge + rights, into the caller's table; it snitches a `CapEvent::Transferred` carrying the badge. The pure derive is `kernel_proc::cap::mint_badged` (host-tested).

The **`MINT`-holder owns the object and sets the child's rights freely** (it is granting authority to *its* endpoint, not attenuating its own). Monotonic narrowing by non-owners is the lever for *client re-delegation* — **deferred**: clients hold no `MINT`, so they cannot mint at all yet.

> **Two rights layers** (see filesystem-design.md → *Two rights namespaces*): the kernel's generic `rights` mask governs **endpoint operations** (below). A server packs its own **object rights** (e.g. file `READ`/`WRITE`) into the **badge**, where they are immutable and server-interpreted. Narrowing *object* rights is therefore a server mint, not a kernel derive — until the deferred typed-capability generalization.

## Endpoint rights (the generic mask)
Defined in `snitchos_abi::rights` (single source of truth; `kernel_proc::cap::Rights` wraps them):
- **`SEND`** — may `send`/`call` on this endpoint (client side).
- **`RECV`** — may `receive` on this endpoint (server side; normally held only by the server).
- **`MINT`** — may derive badged children of this cap.

A typical FS *client* cap is `SEND`; the server holds `RECV | MINT`. (A `GRANT` right gating *general* `send`-carries-caps is **deferred** — v0.9c transfers caps on the reply path only; see below.)

## Cap-transfer — on the reply path (v0.9c)
A `reply`/`reply_recv` may carry one capability to hand back to the caller: the cap handle rides in **`a6`**, the kernel **moves** it out of the server's table into the caller's, and the caller's `call` returns its fresh handle in **`a5`**. This is load-bearing for the FS — `lookup`/`open` is a `call` whose reply hands back a freshly-minted, badged child cap. (The reply cap is the kernel-minted special case of the same move.) General `send`-carries-caps + a `GRANT` gate is the deferred follow-on.

## Revocation
The per-process cap table's **generation** field (`kernel-proc/src/cap.rs`) — given a real job by v0.9b's single-use `consume` — is the kernel-side revocation hook: bump a slot's generation and every outstanding handle to it fails to resolve. Finer liveness (per-badge — e.g. a deleted inode) is revoked in userspace: the server drops the badge→object mapping and replies not-found. **Coarse (whole-cap) revocation is the kernel's; fine (per-object) revocation is the server's.**

# Bulk transfer — cross-address-space copy (option D)

The synchronous answer to "move more than `MSG_WORDS` of data" — names, file bytes, any large arg. A **general RPC primitive**, not FS-specific: the kernel moves opaque bytes between two address spaces; meaning stays in userspace. Chosen over a shared `MemoryRegion` for synchronous RPC because **message passing > memory sharing** across a trust boundary (ownership moves, nothing shared, no over-exposure); the shared-region answer is reserved for the async data plane (below) and only if measurement demands it. Full trade-off: [filesystem-design.md](filesystem-design.md) → *Message framing*.

**Why it isn't a `memcpy`.** The data lives at a VA in the *other* process's address space, which isn't in `satp`. `copy_from_user` (the `SUM`-bit deref) only works for the *active* process. So the kernel **walks the other process's page table** to translate the VA → PA, then touches the bytes via the linear map (`pa_to_kernel_va`, reachable regardless of which AS is active). Buffers spanning pages map to scattered frames with mismatched page offsets, so the copy proceeds page-by-page, advancing by `min(src-bytes-to-page-end, dst-bytes-to-page-end, remaining)`.

**The reply cap *is* the authority.** Two server-initiated syscalls, authorized by the one-shot `Object::Reply { caller }` the server already holds:

```
CopyFromCaller { reply, caller_src_va, len, my_dst_va }   // pull — for write
CopyToCaller   { reply, my_src_va, len, caller_dst_va }   // push — for read
```

The kernel resolves `reply` → the blocked `caller`'s `root_pa`, walks both page tables, copies through the linear map. Properties:
- **Borrows, never consumes** the reply cap — a server may copy many times (header, chunks) before the final `reply`/`reply_with_cap` consumes it.
- **Safe by the rendezvous:** the caller is parked (`block_current`) for the whole window, so its buffer can't mutate mid-copy.
- **Unforgeable scope:** a server can reach *only* a caller awaiting *its* reply — no ambient "copy from any process."
- **Validated per page in each AS:** `ptr+len` no-overflow, wholly in the user half (`user_range_ok`), leaf has `U` + `R` (source) / `W` (dest). Any miss → refuse + snitch (`SyscallRefused`), never a kernel fault.
- **Kernel stays object-ignorant:** it copies `len` opaque bytes; it never decodes the opcode, the badge's `(inode, rights)`, or the FS protocol.

**Split:** the page-walk + chunking + validation is pure host-tested logic in `kernel-core` (`translate(root, va, &dyn PtMem)` + a `copy_across` orchestrator over a byte-copy callback, exercised against a `PtMem` mock); the kernel side wires the two syscalls (reply-cap → `root_pa`, `KernelPtMem` translation, linear-map copy, snitch). Plan: [plans/legacy/v0.10-ramfs.md](../plans/legacy/v0.10-ramfs.md) → Step 3.

# Async-with-data = shared region + notification, behind a channel library
There is no buffered-channel primitive in the kernel. When userspace wants async delivery of data, the pattern is: a shared `MemoryRegion` (the ring buffer) + a `Notification` (the "I added something" poke). The buffering *policy* lives in userspace, where it is testable and replaceable — mechanism in the kernel, policy in userspace.

**The blessed userspace channel library** presents this as an `mpsc`-shaped interface — `Sender` / `Receiver` / `send` / `recv`, ownership moves on send, `recv` blocks until something arrives. Familiar to any Rust programmer. Key design choices:

- **Bounded by default.** Rust's `mpsc` is unbounded; an unbounded queue in an OS is a resource-exhaustion problem, and the backing region is a fixed mapping anyway. SnitchOS channels are bounded — closer to `sync_channel` semantics — a full channel blocks the sender or returns "full."
- **`mpsc` default + `spsc` variant.** `mpsc` (multi-producer, single-consumer) is the general default; an `spsc` variant exists for the audio subsystem's lock-free real-time path, which needs the simpler, faster topology. The kernel does not care — topology is purely a userspace library decision over regions + notifications.

Raw "map a region, take a lock" is possible but unidiomatic — the `unsafe` of IPC. Programs communicate; the library shares memory on their behalf.

# Trace context is kernel-populated in every message
When an operation crosses an IPC boundary, the trace must continue across it — the callee's spans are children of the caller's span. For this, **trace context (current span id, at minimum) travels with every IPC message, populated by the kernel automatically.** Userspace does not opt in and cannot forget; good distributed tracing is ambient.

Consequences:

- The IPC message format reserves a first-class slot for trace context.
- The kernel's IPC path touches the tracing system.
- The mechanism: the current span is per-thread kernel state ("span context lives in the task struct"); the IPC path copies it into the message automatically.
- The v0.1 protocol's `parent_id` field is the seed of this. IPC at v0.9 is where it grows up.

This is the feature that makes the observability pillar impressive: "watch a single trace flow through five userspace services and the kernel" is the demo.

# Compatibility: existing IPC software does not port
A deliberate consequence of the existing non-goals (not POSIX, not Linux-ABI compatible). Three categories:

1. **POSIX IPC software** (pipes, Unix domain sockets, System V shm, signals, `fork`). None of these primitives exist in SnitchOS. This does not "port with a shim" — the model differs. POSIX IPC assumes ambient authority (global namespace, any process can name any socket path); SnitchOS IPC is capability-gated. **Does not run.** Porting it would drag ambient authority back in and actively undermine the capability pillar.
2. **Software written against a higher-level channel/actor abstraction.** *This* has a path: implement that interface on top of SnitchOS channels, the program logic is unchanged. This is why the `mpsc`-shaped interface matters — code that depends on the interface, not the mechanism, is the code that can move.
3. **Rust `std::sync::mpsc` for in-process concurrency.** Not IPC — threads in one address space. Just works; does not touch the kernel boundary.

The compatibility story for IPC is the same as everywhere: **Rust source portability of channel-interface code + WASM, explicitly not ABI/POSIX compatibility.** WASM composes cleanly here — a WASM module talks to the world only through imports, and imports are capabilities, so a WASM module "doing IPC" is just calling an imported channel function.

Mild gravitational pull worth noting: the software that runs well on SnitchOS is the software already written in the "communicate, don't share" style — actor-style services, pipeline-structured programs. The OS rewards the discipline Go was preaching.

# Possible blog post
*"Don't communicate by sharing memory" — at the OS level.* Most readers meet the slogan as Go advice; showing how it reframes when isolation is the default instead of sharing is a fresh angle that ties the IPC design to an idea readers already have opinions about.

# Decisions locked
- Two primitives: synchronous endpoints (workhorse, ✅ v0.9) + notifications (async, payload-free, ✅ v0.12).
- Synchronous is the default IPC primitive; direct context switch on the hot path.
- Payload: small messages copied inline via message registers; large payloads via `MemoryRegion` capability transfer. A message is inline words + capabilities.
- ✅ **(v0.9c)** Endpoint caps carry an immutable, server-chosen **badge** delivered unforgeably to the receiver in `a6`; one endpoint demuxes many objects/clients by badge.
- ✅ **(v0.9c)** `MintBadged` derives badged children; the `MINT`-holder (object owner) sets their rights freely. Object-specific rights (file READ/WRITE) ride in the badge until a typed-capability generalization. Rights bits are the single source of truth in `snitchos_abi::rights`.
- ✅ **(v0.9c)** **Cap-transfer on the reply path** (cap in `a6`, kernel moves it, `call` returns the new handle in `a5`) — required for capability-returning servers (the filesystem's `lookup`/`open`). General `send`-carries-caps + a `GRANT` right is deferred.
- No buffered-channel kernel primitive. Async-with-data = shared region + notification, behind a userspace channel library.
- Channel library: `mpsc`-shaped interface, **bounded by default**, with an `spsc` variant for the audio RT path.
- Trace context is a first-class kernel-populated slot in every IPC message; mechanism is per-thread span context in the task struct.
- Compatibility: Rust source portability + WASM. POSIX IPC explicitly unsupported — a deliberate, accepted cost.

# Open / deferred to v0.9
- Abortable / timeout send semantics for deadlock mitigation.
- Exact message-register count and inline payload size.
- Server loop shape and multi-client handling conventions. (v0.9c demonstrates one: `receive_with_reply` + per-badge demux in a single loop.)
- `GRANT` right + general `send`-carries-caps (v0.9c ships reply-path cap-transfer only).
- Client-side **re-delegation** — a client minting narrower sub-caps; needs the server to grant `MINT` onward, and *then* kernel-enforced monotonic narrowing of the rights mask matters.
- The **typed-capability generalization** (kernel-carried, server-interpreted object rights) — the FS's "#4" evolution; v0.9c keeps file rights in the badge (server-interpreted).
- Badge width/encoding is a *v0.10 (FS)* decision (`inode:u32 | rights:u16 | spare:u16` in the `u64`); the kernel stays opaque.
