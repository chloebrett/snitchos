# 📬 IPC design

*Mechanism in the kernel, policy in userspace. Synchronous by default. Capability-gated. Every message traced.*

Not built until v0.9. Designed now because the IPC message format and the trace-context commitment shape the observability protocol and the kernel boundary.

> **Numbering note:** this page predates two insertions — SMP at v0.6 and preemption at v0.8 — which pushed everything downstream forward. IPC (synchronous endpoints + notifications) now lands at **v0.9**; userspace + capabilities are the shipped **v0.7a/v0.7b**, with **v0.8 preemption** in between. See `docs/roadmap-and-milestones.md` for the current sequence.

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

## Notifications — the async primitive
The physical world is asynchronous — interrupts happen whether or not anyone is ready to receive. So an async primitive is mandatory. But it does not have to be buffered channels; it is a **notification**: async signalling stripped to the bone. Essentially a per-object set of bits. Signalling sets a bit and wakes any waiter; it carries no payload (or one word at most). No arbitrary-message buffer, so no buffering problem. Models interrupts arriving and readiness signals ("data is ready, come do a synchronous receive").

# Message payload: inline copy + region transfer
- **Small messages** are copied inline through a fixed-size set of message registers (a handful of machine words, L4-style). The fast common path.
- **Large payloads** are transferred by granting a `MemoryRegion` capability rather than copying bytes.
- A message is therefore *some inline words + some capabilities*. "Passing data" and "passing a capability" are the same mechanism.

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
- The v0.1 protocol's `parent_id` field is the seed of this. IPC at v0.8 is where it grows up.

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
- Two primitives: synchronous endpoints (workhorse) + notifications (async, payload-free).
- Synchronous is the default IPC primitive; direct context switch on the hot path.
- Payload: small messages copied inline via message registers; large payloads via `MemoryRegion` capability transfer. A message is inline words + capabilities.
- No buffered-channel kernel primitive. Async-with-data = shared region + notification, behind a userspace channel library.
- Channel library: `mpsc`-shaped interface, **bounded by default**, with an `spsc` variant for the audio RT path.
- Trace context is a first-class kernel-populated slot in every IPC message; mechanism is per-thread span context in the task struct.
- Compatibility: Rust source portability + WASM. POSIX IPC explicitly unsupported — a deliberate, accepted cost.

# Open / deferred to v0.8
- Abortable / timeout send semantics for deadlock mitigation.
- Exact message-register count and inline payload size.
- Server loop shape and multi-client handling conventions.
- Endpoint capability rights (send / receive / both).
