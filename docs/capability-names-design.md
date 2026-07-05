# Capability object names — design

**Status:** Proposed.

## The problem

`hold` renders a process's cap table, now legible and color-coded. But the `kind`
column is a *structural* type, and one kind is a black box:

```
│ handle │ kind          │ rights │ badge │
│      2 │ Endpoint      │ 🪴📝   │     0 │   ← which endpoint? the FS? a service? a peer?
```

`TelemetrySink`, `SpanSink`, `Notification` are self-describing — the kind *is* the
purpose. But `Endpoint` names only the mechanism (synchronous IPC), never the
thing on the other end. A process holding three endpoints sees three identical
`Endpoint` rows and cannot tell the filesystem from a logging service from a peer.
A human at the powerbox prompt is flying blind about their own authority.

## The principle this must not break

The capability model's core tenet is **"naming an integer is not authority"** — a
cap is *what you can do* (object + rights), reached by an opaque handle, never by a
name. So the invariant a naming scheme must hold is exact:

> **Names are for *seeing*. Handles are for *doing*.**

A name may be attached to an object and shown to humans, but it must **never**:

- gate an authority decision (rights, not names, decide what you may do), or
- serve as a lookup key (`send("fs", …)` resolving "fs" to an endpoint would
  reintroduce ambient authority — the exact failure mode the model exists to kill).

A name is *display-only metadata*. You still need the cap to act; the name only
tells a human what they're looking at. Cross that line — let a name reach or
authorize an object — and it undercuts everything. Stay on this side of it and it
takes nothing away.

## Prior art

This is not novel, and the precedent is the system SnitchOS's cap model already
resembles. `docs/capability-system-design.md` calls these "Zircon-style sparse
handles"; Zircon (Fuchsia's kernel) has *exactly* this feature:
`zx_object_set_property(handle, ZX_PROP_NAME, …)` puts a short, arbitrary string on
a kernel object, bounded at `ZX_MAX_NAME_LEN` (32 bytes), purely for
debugging/observability. It never affects rights and is never a namespace. Object
names are a settled, proven design in a capability kernel; we are adopting it.

## Why it fits SnitchOS especially well

SnitchOS is observability-first — its whole thesis is *watch authority happen*. A
name on an object is not just prettier `hold` output; if the name rides in the
`CapEvent` wire frames, the entire **derivation tree in Tempo becomes legible**:

```
before:  process 4 transferred endpoint cap_id=4172 to process 7
after:   process 4 transferred the "fs" endpoint to process 7
```

Every grant, mint, and revoke you watch on the wire names what moved. A shell-side
label map (an observer keeping local aliases) can only clarify *your own* `hold`;
an object-name clarifies the trace for *everyone watching the system*. For a kernel
that snitches, naming the objects is the missing piece that makes the snitching
readable.

## Design

### The name lives on the object, set by its creator

A name is a property of the **object**, not the cap. The endpoint table
(`static ENDPOINTS: Mutex<Vec<Endpoint>>` in `kernel/src/trap/ipc.rs`) gains a name
field on each `Endpoint`; `EndpointCreate` takes the name and stores it. Every cap
pointing at that object — the creator's, and every delegated/minted descendant —
resolves the *same* name for display. Delegation copies rights and a handle, not a
name; the name is looked up from the object at render time.

This is the right axis for the observability goal: the name is set once, by
whoever brought the object into being (who knows its purpose), and is visible
everywhere the object is, including down the delegation chain and on the wire.

### Bounded, opaque, non-resolving

- **Bounded**: a fixed max length of **24 bytes**, UTF-8 (decided; Zircon uses 32).
  Fits inline in the object and the wire frame without heap churn; truncate on
  overflow. A `[u8; 24]` + length, not a `String`.
- **Opaque to the kernel**: the name is stored and copied, never parsed, compared,
  or consulted for any authority decision. No code path branches on it.
- **Not a namespace**: there is no "resolve a name to an object" syscall. You reach
  an object only through a cap you hold. The name is metadata attached *to* a
  handle you already have, never a way to *get* a handle.

### The flow, end to end

1. `EndpointCreate(name_ptr, name_len)` — the creator names the endpoint. The
   kernel copies ≤N bytes into the `Endpoint` object.
2. `CapList` / `CapDesc` — the introspection syscall reports each cap's object name
   alongside `kind`/`rights`/`badge` (the spare `CapDesc.reserved` is too small;
   the name needs its own inline bytes).
3. `hold` — the shell adds a `for` (or `name`) column, populated from the object
   name. Unnamed objects render blank.
4. `CapEvent` — the wire frame for grant/mint/revoke carries the name, so the
   collector reconstructs a *named* derivation tree in Tempo. **This is the payoff.**

### How the startup ("fs") endpoint gets its name

The FS endpoint the REPL holds isn't created by the REPL — `init` (or the workload
launcher) `EndpointCreate`s it, delegates `RECV|MINT` to the FS server, and mints a
`SEND` client cap for the REPL. Naming happens at *that* `EndpointCreate`: `init`
passes `"fs"`. The name rides on the object, so the REPL's delegated cap — and the
server's, and every `CapEvent` about it — shows `"fs"` for free. No change to the
delegation path; only the creator names, once.

### Two axes (scope)

- **Object-name** — creator-set, global, travels with the object, appears in
  `CapEvent`s. "What *is* this thing." **In scope.**
- **Holder-alias** — observer-set, local to one process's cap table. "What *I* call
  my handle to it." A distinct, also-legitimate axis. **Deferred** — the
  object-name is higher-value (it makes the wire legible) and simpler (one name,
  set once).

## The one honest caveat: names are assertions, not trust signals

The name is the *creator's* claim about the object. A malicious creator can name an
endpoint `"trusted-payments-api"` to mislead a holder. But note the *shape* of that
risk: it is **phishing** (a misleading label), not **authority bypass**. The holder
still has exactly the rights they were granted; the name authorizes nothing. Zircon
lives with this. The design must state plainly, in code and docs, that **an object
name is never a trust signal** — a program deciding *whether to trust* a cap must
look at where it came from (provenance / the derivation tree), never at the name.
This mirrors the shell's color rule (`docs/` post 42): presentation that carries
weight keys on provenance, never on a self-reported string.

## What it touches

A genuinely cross-cutting change — the reason it's a milestone, not a tweak:

- **`abi`**: `EndpointCreate` gains name args (ptr, len); `CapDesc` and the
  `CapEvent` layout gain an inline name field.
- **kernel**: `Endpoint` struct gains a name; `handle_endpoint_create` copies it in
  (SUM-guarded, bounded); `handle_cap_list`'s `describe` carries it out;
  `emit_cap_granted`/`_transferred`/`_revoked` include it.
- **`protocol`**: the `CapEvent` frame + `OwnedFrame` gain a name; the decoder
  round-trips it.
- **`collector`**: name → an OTLP attribute on the cap span, so Tempo shows it.
- **userspace runtime**: `endpoint_create` binding takes a name; `CapInfo` (what
  `hold` lifts) carries it.
- **stitch**: `hold` renders a `for` column; `native` for creating a named endpoint
  (a future `endpointCreate`/`serve` verb).

## Increments

1. **The object carries a name, and `hold` shows it.** ✅ **DONE.** `CapDesc` gains
   a NUL-padded `name: [u8; CAP_NAME_LEN]` (+ `pack_name`/`name_str` helpers, an
   array `Pod` impl in `hitch-pod`); the kernel `Endpoint` object stores it,
   `EndpointCreate` takes it at `a1`/`a2` (required, UTF-8-validated), and
   `CapTable::describe(name_of)` resolves each endpoint cap's name via a kernel
   resolver reading the endpoint table. Userspace `endpoint_create(name)` +
   `RuntimePlatform::hold` lift it into `CapInfo.name`; `hold` emits a `for` field
   the table renders. `init`/the shared workload endpoint is named `"fs"`. Itest
   `stitch-hold-shows-endpoint-name` asserts the rendered `│ … Endpoint │ fs │ …`
   row on the UART. Host-tested end-to-endpoints (abi, kernel-core describe,
   stitch); 0 missed mutants on the pure helpers; regressions (endpoint-create,
   init-fs, grant/revoke, hold-lists) all green.
2. **`CapEvent`s carry the name.** Thread the name through the three emit sites +
   the wire frame + the collector's OTLP mapping. The observability win: a *named*
   derivation tree in Tempo. Itest asserts a `CapEvent` frame carries the name.
3. **(Deferred)** Holder-alias (local rename); naming non-endpoint objects if a case
   arises (their kinds are already descriptive, so low priority).

## Decisions

- **Name length: 24 bytes**, UTF-8, inline (`[u8; 24]` + length). Truncate on
  overflow. Kept tight because the name is copied per `CapEvent`.
- **Required at `EndpointCreate`**, and **create-time only** (immutable — no later
  `set_name`). A mandatory, unchanging name is the cleanest observability signal
  and means every endpoint on the wire is named. Existing `EndpointCreate` callers
  must be updated to pass one (compiler-enforced — the signature changes).

## Open questions

- **Non-endpoint objects.** `TelemetrySink`/`SpanSink`/`Notification` kinds are
  already self-describing; naming them is possible but low-value. Scope to endpoints
  first.
