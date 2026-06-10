# 🔑 Capability system design

*No ambient authority. Every resource access goes through an unforgeable handle. The project's second pillar.*

Not built until v0.7b. Designed now because it is a compounding decision — the kernel/userspace boundary, IPC, and the syscall surface all assume this shape.

> **Numbering note:** this page predates the SMP insertion at v0.6, which pushed everything downstream forward by one. References below have been updated: capabilities land at **v0.7b** (was v0.6b), the deliberately-wrong first syscall at **v0.7a** (was v0.6a), and IPC endpoints/notifications at **v0.8** (was v0.7). See `docs/roadmap-and-milestones.md` for the current sequence.

# The kernel surface: "invoke a capability"
**Framing: one conceptual operation.** The kernel API is, conceptually, a single operation: *invoke a capability*. "Syscalls" are messages to the kernel's own objects or to userspace services. This is the seL4 framing and it is the story SnitchOS tells about its kernel surface.

**Reality: a small enumerated set.** Mechanically there are a few distinct trap entry points — capability invocation, plus a couple of primitives like yield and a debug/telemetry escape hatch. This is fine; even seL4 has a handful of distinct kernel operations. The conceptual model is "invoke a capability"; the implementation is "a small fixed set." b-framing, c-reality.

This decision *is* the v0.7a → v0.7b narrative: v0.7a builds one ambient syscall deliberately the wrong way; v0.7b refactors it into capability invocation.

# What a capability is

## Sparse handles (Zircon / Fuchsia model)
Each process has a **capability table** (`CapTable`). A capability is an opaque integer handle — a `u32` — like a Unix file descriptor but unforgeable: the kernel validates every invocation against *the calling process's own table*. `handle 3` means nothing except as an index into your process's table; the same number in another process refers to something else or nothing.

Chosen over seL4's CSpace / guarded-page-table model. CSpace is intellectually elegant and supports very fine-grained delegation, but CSpace addressing is one of the hardest things to understand in seL4 and would be a complexity sink that eats milestones. The learning goal is better served by a model that fits entirely in your head. An essay comparing the two models is a good artifact. (This is also the same opaque-integer-ref-validated-against-a-table pattern as protocol string interning.)

## Capability structure
A capability is `{ object: <ref to kernel object>, rights: RightsBitmask }`.

- `object` — a reference to the actual kernel object.
- `rights` — a bitmask (read, write, send, receive, grant, ...).

The same object can be referenced by multiple capabilities with different rights. This is how **attenuation** works: you hold a read-write capability to an object and hand someone else a read-only capability to the same object. You can only ever attenuate (drop rights), never amplify.

# Kernel object types
The set of things a capability can point at, in roughly the order milestones need them:

- **Thread** — a thread of execution (v0.5)
- **AddressSpace** — a virtual address space / page table (v0.4–0.6)
- **MemoryRegion** — a chunk of physical memory that can be mapped (v0.4)
- **Endpoint** — an IPC channel endpoint (v0.8)
- **Notification** — the lightweight signal primitive, separate from IPC (v0.8)
- **Interrupt** — the right to receive a hardware interrupt (v0.3+)
- **CapTable** — a process's own capability table, so capabilities can be granted and revoked (v0.7b)
- **MemoryRegion** — a chunk of physical memory + the right to map it into an address space. The canonical microkernel cap (seL4's Frame/Untyped). **Deliberately *not* v0.7b** — it has no consumer until something grows or *shares* memory, and a cap with no consumer is machinery for its own sake. Its real reason to exist is shared memory between two processes, so it lands with **v0.8 IPC**. Distinct from an allocator: a `malloc` subdivides bytes *within* memory you already hold (retail); a `MemoryRegion` cap is how you *acquired* that memory and the right to place it in an address space (wholesale). See [v0.7b plan](../plans/v0.7b-capabilities.md) for why it's excluded.
- **TelemetrySink** — **the v0.7b first object** (confirmed, no longer provisional). A boolean cap: "may emit telemetry." A userspace component can reach the collector *only* if it holds this cap — observability becomes capability-governed; you can see and control who is allowed to snitch, and a process with no cap has no telemetry egress at all.
  - **Identity is kernel-stamped, never a parameter.** The frame's attribution (`thread.name` / owning identity) is set by the kernel from the calling process — so a process *cannot* emit *as* anyone else. This makes trace-forgery impossible *by construction*, which is strictly better than making non-forgery a granted right (a right can be over-granted; "identity isn't a syscall argument" can't be). There is therefore **no `EMIT_AS_ANY` right** — it was considered and rejected.
  - **Register-on-emit, no separate right.** Registration is *not* a distinct right or distinct call — register and emit are the same operation from userspace (this was considered as `REGISTER` and rejected). **Scope note:** the conceptual end-state is `emit(name, value)` with the kernel interning the name on first use, but a user-passed *name* means the kernel reads a user string buffer — which needs `SUM` + user-pointer validation, deliberately deferred past v0.7a. So **v0.7b ships the value-only form**: the `TelemetrySink` cap is *bound at creation* to a kernel-registered counter, and `invoke(handle, value)` emits to it — no string crosses the boundary, `SUM` stays `0`. User-named metrics arrive when `SUM`/user-buffer-copy lands (naturally alongside v0.8 IPC message buffers). The capability naming the sink (rather than the user passing a name) is in fact *more* capability-idiomatic.
  - **Rights are vacuous-but-present at v0.7b.** One `EMIT` bit, one method. Attenuation and multi-method dispatch are real machinery the cap *system* carries, but `TelemetrySink` does not exercise them — that is deliberate. The skeleton is proven here against the minimal object; richer facets are exercised by objects that have a genuine reason to be rich (`Endpoint` transfer at v0.8, `File` read-only-vs-read-write attenuation at v0.10). Do not inflate `TelemetrySink` to make v0.7b feel substantial.

# The kernel snitches freely; userspace needs a cap
Tension: if telemetry is a capability, the kernel needs that capability to emit its own spans — but the kernel emits telemetry from v0.1, long before capabilities exist (v0.7b).

Resolution: the **kernel's own** telemetry emission is ambient and direct — it is the kernel, it is allowed to do anything. `TelemetrySink` capabilities govern only **userspace components** emitting telemetry. The rule: *userspace needs a capability to snitch; the kernel snitches freely.* Capabilities govern the boundary, not the kernel's internals. Nothing about v0.1 changes.

# Capability operations (sketch — detailed at v0.7b)
- **Invoke** — the one operation; do the thing the capability authorizes.
- **Grant / transfer** — pass a capability to another process, through IPC (see IPC page).
- **Attenuate** — derive a weaker capability (fewer rights) to the same object.
- **Revoke** — invalidate a capability. **Single-cap revocation is already seeded:** the v0.7b `Handle` generation tag *is* the primitive — bump a slot's generation and every stale handle to it fails `resolve`. **Recursive revocation** (kill a cap and everything derived from it) additionally needs the kernel to track derivation edges (an in-kernel CDT) — deferred until derivation exists (v0.8 transfer). The host-reconstructed tree visualises revocation (subtree goes dark) but never *enforces* it. Strategy in detail (membranes / time-bounds) still deferred; the generation tag is the fallback that's free today.
- Capabilities cannot be forged or synthesized from thin air. The kernel mediates every transfer.

# Bootstrap
Root capabilities are granted to the `init` process only. All other capabilities flow from there, by grant and attenuation, through IPC. This is the delicate part of any capability system — detailed when v0.7b is planned.

# Observability angle: authority as a first-class observable

The two pillars meet here. Every **authority decision** is snitched, and
the security policy itself becomes a *derived view* in the observability
stack. This is where capabilities stop being plumbing and become the
demo.

## What gets snitched (and what doesn't)
Decisions worth observing are the **rare, high-value** ones, not the hot
ones:

- **Grant** — the kernel snitches when it hands out a capability. Grants
  *create* authority (invocations merely *exercise* it), so the grant
  stream is the richest, most security-relevant signal on the wire. The
  kernel snitches its own grants freely (consistent with "kernel snitches
  freely"); no cap needed to do so.
- **Deny** — a refused invocation (bad/stale handle, missing right) is an
  observable event. This is the cap twin of v0.7a's `faults_total`.
- **Invoke** — observed **by effect** (the thing the invocation did,
  e.g. the emitted metric), *not* as a per-call event. Invokes can be
  hot; a per-invoke event would be syscall-volume noise. A counter is the
  most we'd add.
- Later: **Revoke**, **Transfer / Attenuate** — the edges that turn the
  event log into a tree (see below).

**Sequencing (v0.7b):** `cap.grants_total` + `cap.denied_total`
**counters first** (cheap, no wire change), then a dedicated `CapEvent`
**frame** (`Granted | Invoked | Denied`, later `Revoked | Transferred`)
that carries the fields a counter can't.

## The capability derivation tree — emitted, not stored
Capabilities are *derived* from one another: the kernel mints a root and
grants it to `init`; `init` grants / attenuates / transfers to produce
children. Those parent→child edges form the **capability derivation
tree** (seL4's CDT). SnitchOS handles this tree the same way it handles
the trace tree:

> **The kernel does not store the tree. It emits the events; the host
> reconstructs it.** Exactly as the kernel emits span start/end and Tempo
> builds the trace tree — never storing it in-kernel. The cap derivation
> tree is to *authority* what the trace tree is to *execution*.

This keeps the kernel microkernel-minimal and makes the security policy a
view in the collector. For host reconstruction to work, each capability
needs a **global telemetry id** — a monotonic `u64`, minted like a span
id, **distinct from the per-process `Handle`** (the handle is local and
ambiguous across processes; the host needs a global id to thread edges).
The `CapEvent` frame carries this id + a parent-cap-id field from the
start, even though at v0.7b every cap is a root with no parent — cheap to
include, expensive to retrofit (same logic as the generation tag).

**Two trees, don't conflate them:** the *enforcement* tree (in-kernel
CDT, built only when revocation lands) is what the kernel trusts to
revoke; the *observability* tree (host-reconstructed from the stream) is
for visualisation and audit. They should agree — "does the observed tree
match the kernel's enforcement state?" is itself a snitch-on-the-snitch
consistency check — but enforcement can **never** trust the host tree.

## Visualisation (v0.8+, once derivation makes it a real tree)
At v0.7b the "tree" is a depth-1 star (kernel → bootstrap caps); it earns
a visualiser only once **v0.8 transfer/attenuate** create real edges.
Then, mostly in native Grafana:

- **Structure** — Grafana **Node Graph** panel over a collector JSON
  endpoint (Infinity / JSON datasource); nodes by object type + holder,
  edges by rights.
- **Cross-links** — Tempo span links / Grafana data links templated on
  the cap id: jump from an invocation span to the cap graph filtered to
  the authority that permitted it.
- **Revoke goes dark** — node colour driven by a status field.
- **Replay** — *stepwise* via the time picker ("as of time T") works
  natively; *smooth animated* replay wants a thin bespoke viewer over the
  **same** `CapEvent` stream (a rendering choice, not a data-model
  change).

# Decisions locked
- Kernel surface: "invoke a capability" framing; small enumerated set in reality.
- Sparse handles (Zircon model): per-process `CapTable`, opaque `u32` handles, kernel validates against the caller's table. Not seL4 CSpace.
- Capability shape: `{ object, rights }`. Attenuation by holding multiple caps to one object with different rights.
- Kernel object set as listed above.
- `TelemetrySink` is **confirmed** as the v0.7b first object: a boolean "may emit" cap, kernel-stamped identity (no `EMIT_AS_ANY`), register-on-emit (no separate `REGISTER` right). Not provisional.
- `MemoryRegion` is **deferred to v0.8** — no consumer until shared memory exists. It is not redundant with an allocator (wholesale vs. retail); it is the substrate an allocator stands on and the only mechanism for sharing.
- Kernel telemetry is ambient; userspace telemetry is capability-governed.
- Handle layout carries a **generation tag** from v0.7b (slotmap-style `index + generation`) even though nothing revokes yet — cheap now, expensive to retrofit; makes later revocation "bump the slot's generation."

# Open / deferred to later
- Revocation strategy in detail (the generation-tag seed is in place at v0.7b; the *policy* — membranes vs. time-bounds — is deferred).
- Cap **transfer / grant between processes** — impossible at v0.7b (one process, no IPC). Lands at **v0.8** with `Endpoint`.
- Whether the kernel adopts capabilities internally, and where.
- Rights bitmask exact contents beyond `EMIT` (richer bits arrive with `Endpoint`/`File`).
