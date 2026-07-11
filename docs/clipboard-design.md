# Design: the clipboard as a principled primitive

**Status**: Vision / design note (unbuilt). A candidate showcase feature that falls
out of the OS's existing convictions rather than a new mechanism. Surfaced while
designing the [stim grammar](../plans/stim-grammar.md) (yank/paste), which flagged
"taint-aware yank/paste = the IFC axis" — this doc is that thought, generalized to
the whole system.

## The thesis

The clipboard is the single least-principled primitive on the Linux desktop. Copy/
paste — "move a value from here to there, later" — is one of the most-used
operations a person performs, and it is:

- **untyped** — opaque bytes plus weak, out-of-band MIME "target atoms";
- a **global singleton**, owned by the window manager, with transient ownership
  (close the source app and the clipboard evaporates unless a manager is hoarding it);
- **history-less** — you get exactly one item; multi-item history is a third-party
  hack layered on top;
- **provenance-less** — a pasted value has no memory of where it came from;
- **unobservable** — data crosses the whole trust boundary of your session and
  nothing records it;
- **authority-blind** — it can only move *data*, never *capability*, safely; and
- **schismatic** — vim's registers and the system clipboard are two disjoint worlds
  bridged by the fiddly `+`/`*` registers.

Here is the observation that makes this worth a design note: **every one of those
jank points is a property SnitchOS already has the primitive to fix.** The clipboard
is not a feature to bolt on — it is a *lens*. It shows whether the OS's four
convictions (typed values, explicit authority, total observability, data provenance)
actually **compose**. If they do, you get a clipboard better than any desktop's, for
free. If they don't, the clipboard is exactly where you find out.

## The mapping (each Linux jank → a SnitchOS primitive)

| Linux clipboard problem | SnitchOS primitive that fixes it |
|---|---|
| Untyped bytes + weak MIME atoms | **Hitch** typed values + schemas ([data model](typed-processes-and-the-data-model-design.md)) |
| One global buffer, WM-owned, transient | **Capability-scoped** clipboards — many, isolated, persistent-as-the-holder |
| No history (needs a manager hack) | It's a **service** with a log — history is native, not bolted on |
| No provenance | Every entry carries `{source process, cap, region, time}`, **transitively** |
| Invisible | Copy/paste are **spans**; the history *is* a trace on the wire |
| Data-only (can't move authority) | Entries carry **caps**; copy attenuates; paste is an observable transfer |
| Secrets leak (password sits in the buffer) | **IFC labels** — a secret entry is concealed: not historized, not pasteable into a public sink |
| vim registers ≠ system clipboard | **One service** — registers are named slots in it; no two-worlds split |

The table is the whole idea. The rest of this doc is architecture.

## What the clipboard *is* on SnitchOS

Not a buffer. A **service** (an actor, per the [actor-model design](userland-text-streams-and-the-actor-model-design.md)) that owns an append-only history of **entries**, reached over capability-mediated IPC.

An **entry** is an immutable, content-addressed record:

```
Entry {
  value:      Hitch,          // the payload as a typed value, not bytes
  schema:     Schema,         // its type — so paste is type-checked, previews are structured
  provenance: { process, source_cap, region, time, parents: [EntryId] },
  label:      IfcLabel,       // integrity/secrecy — governs where it may flow
  caps:       [Capability],   // optional: authority the entry carries (e.g. a file cap)
}
```

- **copy** = a typed IPC message to the service: `(value, schema, provenance, label, caps?)`. The service appends an entry and returns its id. Gated by a `Clipboard` cap carrying a `COPY` right.
- **paste** = request an entry (the latest, by index, or by a query) → the service returns the typed value / transfers the caps. Gated by `PASTE`.
- **multiple clipboards fall out of capabilities.** There is no global singleton: a per-session clipboard, a shared team clipboard, a scratch clipboard are just different service instances (or badged caps into one). Who can copy/paste into which is *who holds which cap* — the "one buffer owned by the WM" problem dissolves into the authority model.
- **history falls out of the service** keeping the log. **Provenance falls out of the entry** carrying its source. **Observability falls out of the OS** — every copy/paste is a span, so the history is reconstructable from the wire. None of these are features; they're what you get for making the clipboard a proper SnitchOS citizen.

## The three depths (where it stops being a clipboard and starts being the OS)

**1. Provenance graph (the data-flow DAG).** Entries link to their parents: paste
A into document B, then copy from B, and the new entry's provenance includes A. So
the clipboard is not a ring buffer — it's a **system-wide data-flow graph**, and
because every copy/paste is observed, that graph is on the wire. "Where did this
line in my config come from?" becomes a query that walks the lineage back through
every copy. This is the [data-provenance](typed-processes-and-the-data-model-design.md)
angle realized as a thing you use fifty times a day. No desktop has this.

**2. Information flow (the taint axis).** Every entry carries an IFC label. The
service enforces flow rules on *paste*: pasting a high-secrecy value into a
low-secrecy sink (a log, a world-readable file) is a checked flow — refused, or
downgraded, and **snitched**. This is macOS's "concealed clipboard" flag for
passwords, except principled: a secret entry isn't stored in searchable history at
all, or is stored under a lock, and can only be pasted into a sink cleared for it.
The [stim grammar](../plans/stim-grammar.md) already names this as the IFC axis for
yank/paste; the clipboard service is where it lives for the whole system.

**3. Capability transfer.** An entry can carry a *capability*, not just data. Copy a
file → the entry holds a file cap. Paste = a cap transfer, an observable `CapEvent`.
And copy can **attenuate**: you copy a read-only *view*, so the paste cannot grant
more authority than you had. "Copy this file into that folder" becomes a real,
least-authority, auditable grant — not a byte-blit.

## The stim / vim integration (unifying the two worlds)

vim keeps registers (unnamed, numbered `0`–`9`, named `a`–`z`, the `+`/`*` bridge)
in a world entirely separate from the system clipboard — the source of endless
`set clipboard=unnamedplus` grief. On SnitchOS there is **one service**, and vim's
registers are just **named slots in it**:

- stim `y` (yank) → copy to the service, provenance-tagged (this file cap, these
  lines, now). `p` → paste the latest. `"a` → a named slot. `dd` → delete *into* the
  numbered-register history (vim's small-delete ring — which is literally
  emacs-kill-ring history, native here).
- **structured yank**: because stim's document is a typed Hitch value (the
  schema-driven edit side in `stim-design.md`), you can yank a *typed sub-value* —
  copy a table row *as a row*, paste it type-checked into another table — not as
  reflowed text. The clipboard being typed is what makes structured editing
  composable across documents.
- the **clipboard-manager UX** (history, search, pin, preview, the provenance view)
  is a client that holds a clipboard cap and lists/queries the service. In stim it's
  a mode; system-wide it's a shell verb. Same service, same history, no split.

## Prior art (worth mining, per the ask)

- **Emacs kill-ring** — the original multi-item clipboard-with-history (a ring of
  kills, `M-y` to cycle). The history/ring model is 40 years old and still better
  than the system clipboard; SnitchOS makes it the *system* model.
- **Plan 9 plumber** — the closest spiritual ancestor: typed, rule-routed
  inter-program data with a notion of where data came from and where it should go.
  Copy/paste as *typed routing*, not byte-blit. Read this one carefully.
- **macOS clipboard managers** (Paste, Maccy, Raycast, Alfred, Pastebot) — the UX
  target: searchable unlimited history, pinned favorites, type-aware previews
  (images/files/colors/links/code), per-app filtering, and the **concealed-type
  convention** (`org.nspasteboard.ConcealedType`) that password managers set so
  secrets skip history. That convention is exactly an IFC label — do it for real.
- **Windows Clipboard History** (`Win+V`) — multi-item, pinned, synced; proves the
  mainstream appetite for history+pin.
- **Content-addressed / immutable value stores** — entries are immutable Hitch
  values; identical copies dedupe by hash, and an entry id is a stable handle.

The synthesis: **kill-ring's history + plumber's typed routing + macOS's UX +
SnitchOS's provenance / capabilities / observability / IFC.** Each contributor
solves one axis; the OS is what lets them compose into one primitive.

## Phasing (soft → hard, riding the axes — the stim pattern)

Same discipline as everything else on this project: ship the soft form where it's
cheap, wire the hard form as its axis lands. Even Phase 1 alone beats Linux.

- **P1 — the service (soft, buildable now).** A clipboard actor holding a history
  of *text* entries + provenance metadata (source process/cap/region/time), copy/
  paste over IPC, a `Clipboard` cap gate, and a span per operation. stim `y`/`p`/
  named registers talk to it. This already delivers: history, provenance,
  capability-scoping, observability, and one unified register/clipboard world.
- **P2 — typed entries.** Hitch values + schemas → structured yank/paste and typed
  previews. Rides the typed-processes / data-model work.
- **P3 — IFC labels.** Concealed/secret entries, flow-checked paste. Rides the IFC
  axis (the taint story the stim grammar and this doc both point at).
- **P4 — capability entries.** Copy/paste a cap, attenuated on copy. Rides the cap
  story (mostly already there — this is `grant`/`mint` through the clipboard).
- **P5 — the manager.** Search, pin, preview, and the **provenance-graph view** — a
  client over the service + the trace. This is where the "data provenance you can
  *watch*" demo lives.

## Naming (deferred)

Wants a real name (it's a first-class service, not "the clipboard"). Candidates in
the house style: *shelf* (things set aside), *stash*, *hitch-board* (it holds
hitched values), or lean into the snitch/observability theme. Bikeshed later; the
`snip` name is taken (the xtask staging tool).

## Why this is worth remembering (not building yet)

It's tangential to the current stim grammar / spawn work and should not preempt it.
But it's a rare thing: a **familiar, universally-wanted feature** (everyone gets
copy/paste) that is *also* a clean showcase for all four of the OS's convictions at
once, and it reuses mechanisms that are mostly already designed (Hitch, caps, IFC,
observability, the actor model). When there's an appetite for a demo that lands with
people who don't care about microkernels — "your clipboard remembers everything, knows
what everything is, tells you where it came from, and won't leak your password" — this
is that demo.

---
*Delete this file if the idea is abandoned or absorbed into another design.*
