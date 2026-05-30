# 🔑 Capability system design

*No ambient authority. Every resource access goes through an unforgeable handle. The project's second pillar.*

Not built until v0.6b. Designed now because it is a compounding decision — the kernel/userspace boundary, IPC, and the syscall surface all assume this shape.

# The kernel surface: "invoke a capability"
**Framing: one conceptual operation.** The kernel API is, conceptually, a single operation: *invoke a capability*. "Syscalls" are messages to the kernel's own objects or to userspace services. This is the seL4 framing and it is the story SnitchOS tells about its kernel surface.

**Reality: a small enumerated set.** Mechanically there are a few distinct trap entry points — capability invocation, plus a couple of primitives like yield and a debug/telemetry escape hatch. This is fine; even seL4 has a handful of distinct kernel operations. The conceptual model is "invoke a capability"; the implementation is "a small fixed set." b-framing, c-reality.

This decision *is* the v0.6a → v0.6b narrative: v0.6a builds one ambient syscall deliberately the wrong way; v0.6b refactors it into capability invocation.

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
- **Endpoint** — an IPC channel endpoint (v0.7)
- **Notification** — the lightweight signal primitive, separate from IPC (v0.7)
- **Interrupt** — the right to receive a hardware interrupt (v0.3+)
- **CapTable** — a process's own capability table, so capabilities can be granted and revoked (v0.6b)
- **TelemetrySink** — *(provisional)* a capability to emit telemetry. A userspace component can only emit telemetry if it holds this capability — observability becomes capability-governed, and you can see and control who is allowed to snitch. **Flagged as provisional**: revisit if it does not pay off in practice; telemetry could instead stay an ambient kernel service.

# The kernel snitches freely; userspace needs a cap
Tension: if telemetry is a capability, the kernel needs that capability to emit its own spans — but the kernel emits telemetry from v0.1, long before capabilities exist (v0.6b).

Resolution: the **kernel's own** telemetry emission is ambient and direct — it is the kernel, it is allowed to do anything. `TelemetrySink` capabilities govern only **userspace components** emitting telemetry. The rule: *userspace needs a capability to snitch; the kernel snitches freely.* Capabilities govern the boundary, not the kernel's internals. Nothing about v0.1 changes.

# Capability operations (sketch — detailed at v0.6b)
- **Invoke** — the one operation; do the thing the capability authorizes.
- **Grant / transfer** — pass a capability to another process, through IPC (see IPC page).
- **Attenuate** — derive a weaker capability (fewer rights) to the same object.
- **Revoke** — invalidate a capability. Revocation strategy (membranes / generation numbers / capability lists / time bounds) is its own design discussion, deferred to v0.6b. Earlier conversation leaned: membranes as default, time-bounds as attenuation, generation numbers as fallback.
- Capabilities cannot be forged or synthesized from thin air. The kernel mediates every transfer.

# Bootstrap
Root capabilities are granted to the `init` process only. All other capabilities flow from there, by grant and attenuation, through IPC. This is the delicate part of any capability system — detailed when v0.6b is planned.

# Observability angle
Every capability invocation is observable — a span or event. "Watch every authority decision in the system" is a natural SnitchOS demo and ties the two pillars (observability + capabilities) together directly.

# Decisions locked
- Kernel surface: "invoke a capability" framing; small enumerated set in reality.
- Sparse handles (Zircon model): per-process `CapTable`, opaque `u32` handles, kernel validates against the caller's table. Not seL4 CSpace.
- Capability shape: `{ object, rights }`. Attenuation by holding multiple caps to one object with different rights.
- Kernel object set as listed above.
- `TelemetrySink` as a capability: **provisional**, revisit if it does not pay off.
- Kernel telemetry is ambient; userspace telemetry is capability-governed.

# Open / deferred to v0.6b
- Revocation strategy in detail.
- Bootstrap sequence in detail.
- Whether the kernel adopts capabilities internally, and where.
- Rights bitmask exact contents.
