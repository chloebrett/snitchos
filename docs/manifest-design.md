# 📇 Manifest v2 — the authority-description language

*One language for the authority a program needs: **declared** by the child, **satisfied** by a parent, **resolved** by name. The kernel keeps its positional delegation mechanism and stays name-blind — the naming is a userspace contract carried by the program's own manifest.*

**Status: design.** Redesign item **#2** (typed, named startup ABI). This is the
**highest-fan-out** design in the project — five consumers wait on it (see below) —
so per [design-explorations-seven-questions.md](design-explorations-seven-questions.md)
Q4/coda, the *design* is worth doing now even though the *build* is deliberately thin.
It extends two things that already partly exist: the shipped `hitch` **Manifest**
(`hitch/src/lib.rs:168`) and the designed-but-never-built **BootInfo** startup page
([capability-system-design.md:64](capability-system-design.md)). It contradicts
neither; it makes the `uses` row *typed* and kills the positional
`delegated_handle(i)` contract.

Related: [capability-system-design.md](capability-system-design.md),
[typed-processes-and-the-data-model-design.md](typed-processes-and-the-data-model-design.md),
[supervision-design.md](supervision-design.md) (a consumer),
[language-design.md](language-design.md) (Stitch `uses`), and the source analysis in
[design-explorations-seven-questions.md](design-explorations-seven-questions.md) Q1/Q2/Q5.

---

## The problem: two half-built halves that don't meet

**The declaration half exists but is untyped.** `#[entry(in, out, uses)]`
(`user/macros/src/lib.rs:131`) emits a `ConstManifest { input, output, uses }` into a
`.snitch.iface` ELF note; the FS seed lifts it to a `user.iface` xattr
(`user/fs/build.rs`), served over `GetXattr`; and Stitch↔Stitch `~>` structurally
typechecks `input`/`output`. But **`uses` is `Vec<String>`** — bare effect names, "a
soft hint that drives no kernel grant" (Q1). A program *says* it needs the FS; nothing
acts on it.

**The delivery half is positional and hand-hardcoded.** The startup ABI hands a child
its bootstrap caps in registers — `__snitchos_start(telemetry, span, endpoint)` at
a0/a1/a2 — and parent-delegated caps land at `delegated_handle(i) = 2 + i`
(`user/runtime/src/lib.rs:134`). So `fs-client` reads its endpoint as
`Endpoint::from_raw_handle(delegated_handle(0))` — a magic index the program and its
spawner *independently* hardcode. This is exactly the positional contract capabilities
are supposed to abolish, reintroduced at the boundary. The designed fix — an
auxv-shaped `BootInfo` page — was deferred at v0.8 and never built.

**Manifest v2 joins them:** the typed declaration *is* what the satisfier reads to
decide grants, and the child resolves each grant *by the name it declared*, not by a
magic integer. One language, two ends, kernel untouched.

## What exists today (so we extend, not reinvent)

| Piece | Where | State |
|---|---|---|
| `Manifest { input: Option<TypeSchema>, output: TypeSchema, uses: Vec<String> }` | `hitch/src/lib.rs:168` | shipped; `uses` untyped |
| `ConstManifest` (const-buildable, `uses: &'static [&'static str]`) | `hitch/src/lib.rs:304` | shipped |
| `encode_manifest → [u8; MANIFEST_BYTES=1024]`, 4-byte length prefix | `hitch/src/lib.rs:397` | shipped |
| `#[entry(in, out, uses)]` → `.snitch.iface` note | `user/macros/src/lib.rs:131` | shipped |
| `.snitch.iface` → `user.iface` xattr, served via `GetXattr` | `user/fs/build.rs`, `fs-proto` | shipped |
| Positional startup: bootstrap@0/1, delegated@`2+i` | `user/runtime/src/lib.rs:134` | shipped; the thing we replace |
| `BootInfo` auxv-shaped startup page | `capability-system-design.md:64` | **designed, never built** |
| `Spawn`/`SpawnImage` handle-array delegation (`[u32;N]`, all-or-nothing) | `abi/src/lib.rs:125,214` | shipped; **unchanged by this design** |
| Attenuation invariant ("only ever attenuate, never amplify") | `capability-system-design.md:29` | documented |

## Five consumers — why it's one language, not five

The whole reason this is high-fan-out: the same authority-description language serves

1. **Spawn / redesign #2** — the base case: a child declares its needs; a parent
   satisfies them at spawn.
2. **The shell `~>` grant step** — the shell reads a program's manifest and decides
   which of its caps to delegate; *watch least-authority happen* (the explicit-authority
   shell idea) is this, made visible.
3. **The supervision satisfier** ([supervision-design.md](supervision-design.md)) — a
   `ServiceSpec.needs` *is* a Slot list; restart re-satisfies it.
4. **Checkpoint petitions** (axis 6) — a serialized process's authority is its manifest
   plus provenance records; restore re-satisfies (see below).
5. **Stitch `uses` rows** (Q5) — a function's effect row *is* its slot list; the same
   vocabulary spans language effects, manifest slots, and kernel object kinds.

Plus the extension consumers: IFC declassifier grants (axis 4) and budget sizes
(axis 5) ride the Slot's `constraints` field without changing the schema.

---

## The core: the `Slot` type

Promote `uses: Vec<String>` to a list of typed **authority slots**:

```rust
struct Slot {
    name:        Str,                   // the CHILD's local role name: "fs", "clock"
    object:      ObjectKind,            // Endpoint | Notification | TelemetrySink | …
    rights:      Rights,                // requested mask (SEND, RECV|MINT, …)
    // --- deferred (rich vN, not the thin build) ---
    protocol:    Option<TypeSchema>,    // endpoints: request→response shape, structural
    optional:    Bool,                  // required (spawn fails unsatisfied) vs optional
    constraints: List<(Str, Value)>,    // extension point: badge, clearance, budget size
}
```

Three load-bearing choices (Q1):

- **Names are *local roles*, not global service names.** A slot says "I need *an*
  endpoint speaking this protocol, which I'll call `fs`" — never "connect me to
  `/services/fs`." Global naming is the *satisfier's* business. Keeping it out of the
  manifest is what lets the same program run unchanged against the real FS, a
  read-only proxy, a sandbox subtree, or a test double (the interposition axis depends
  on this property).
- **Protocol compatibility is structural** — a `TypeSchema` over the request/response
  sum, matching the shipped `~>` check, cross-language by construction. (Deferred field.)
- **`constraints` is the extension point** the other axes plug into: budgets add
  `("budget.ticks", n)`, IFC adds `("clearance", label)`. The schema itself never
  changes for them — which is why they're deferrable without repainting the type.

### Thin-build scope (per coda Tier-3 #4)

The design presents the full `Slot`; the **build ships only `{ name, object, rights }`**
— enough to kill the positional contract and satisfy consumers 1–3. `protocol`,
`optional`, and `constraints` wait until axes 4/5/6 exist to *demand* them. This is the
project's own second-pass discipline applied to its own proposal: don't build the
general shape until a second consumer reveals the real variation. Encode the fields
now (TLV, append-only — § Versioning) so adding them later is additive, but don't wire
them.

---

## Delivery: the child's own manifest is the BootInfo page

The key simplification, and why **no new kernel mechanism is needed**. Two options were
on the table:

- **(A) Explicit BootInfo page** (the deferred auxv design): the parent writes a
  `[(name, handle)]` page into the child's memory; the kernel copies it in. Flexible,
  but needs a delivery mechanism (a page copy or a new syscall).
- **(B) Manifest-as-index** — *recommended for the thin build*. The child **already
  carries its own manifest** (compiled into `.snitch.iface`), so it already knows its
  slot names *and their canonical order*. The satisfier reads that same manifest,
  satisfies the slots **in order**, and delegates the resulting handles via the
  existing `Spawn` handle array. The child's runtime resolves `name → slot index → 
  delegated_handle(index)`.

Under (B) the wire mechanism is still positional — but the position is now derived from
a **declared, single-source-of-truth manifest** that both ends read, instead of a magic
`2` hardcoded twice. That is what kills the fragile contract. Concretely:

```rust
// child, generated by #[entry(needs = [...])]: a compile-time name→index table
static SLOTS: &[SlotDecl] = &[ /* fs, clock, … in declaration order */ ];

// runtime accessor — resolves the role name against the child's own manifest
let fs: Endpoint = bootstrap().get::<Endpoint>("fs")?;   // → delegated_handle(0)
// vs today's  Endpoint::from_raw_handle(delegated_handle(0))  // magic index
```

The `#[entry]` macro (which already emits the `.snitch.iface` note) also emits the
`SLOTS` const, so the runtime resolves names with **no ELF self-read**. Ordering is the
contract: *the parent delegates handle `2 + i` for slot `i` of the child's manifest.*
Optional slots that go unsatisfied delegate a null-handle sentinel at their index so
indices stay aligned (a rich-build concern; thin build is all-required).

The two **bootstrap caps** (telemetry@0, span@1) stay universal and *undeclared* — every
process is born observable regardless of its manifest (the observability floor). The
manifest declares only program-specific authority, at handles `2+`.

Option (A) remains the richer future (decouples names from order, lets a satisfier pass
extras), but (B) delivers redesign #2's entire value on today's kernel.

## Satisfaction: strictly userspace, kernel keeps its mechanism

The **satisfier** (init, the shell, a supervisor, any parent) is a userspace library:

```rust
fn satisfy(child_manifest: &Manifest, from: &CapTable) -> Result<Vec<Handle>, Unsatisfied>;
```

1. For each required slot, find one of the satisfier's *own* caps matching `object` and
   carrying at least `rights`. Where the slot asks for **less** than the satisfier holds,
   mint an **attenuated/badged** child cap first — upholding "only ever attenuate, never
   amplify" (`capability-system-design.md:29`) at the satisfaction boundary.
2. **All-or-nothing**: any required slot unsatisfiable → the whole spawn is refused
   (snitched), matching `Spawn`'s existing delegation semantics.
3. Call `Spawn(program, handles)` with the handle array in slot order — the kernel ABI
   is **unchanged**. The kernel validates each handle against the satisfier's table
   (it already does) and delegates; it never learns what the names mean
   (mechanism-not-meaning, consistent with the userspace-defined-metrics direction).
4. Emit a **named grant record** per satisfied slot as telemetry: `granted fs ⟵ cap 41`.
   The v0.13 cap-id spine already carries stable `cap_id`/`parent_cap_id`, so this is a
   pure annotation — the wire shows *named* delegation instead of anonymous transfers.

**⚖ Open decision:** is the grant record satisfier-emitted **telemetry** (recommended —
kernel stays name-blind) or a new **kernel frame**? Telemetry keeps the kernel out of
the naming business and costs nothing; a kernel frame would be authoritative but drags
names into the kernel.

## The checkpoint extension (axis 6)

A checkpoint's authority = the original manifest **plus** runtime-acquired holdings. The
manifest slots re-satisfy by name (restart and restore are the same `satisfy` call — see
[supervision-design.md](supervision-design.md)). Caps a process picked up *at runtime*
(a badged file cap from the FS, a reply-path transfer) have no slot, so they serialize as
**provenance records** — derivation chains via `parent_cap_id`, which the spine already
stores — and the restoring authority decides policy per record (re-petition the same
server by protocol + badge, or drop). This is the honest research edge: *named slots
restore cleanly; anonymous acquisitions restore only as well as their provenance is
interpretable.*

## The vocabulary table (Q5) — one set of names, three scopes

This doc is the home for the mapping the effect-system work needs. One vocabulary spans
Stitch effect names, manifest slot roles, and kernel object kinds, so purity/authority is
"one concept at three scopes":

| Stitch effect (`uses`) | Manifest slot `object` | Kernel `ObjectKind` | Nondet? |
|---|---|---|---|
| `Telemetry` | `TelemetrySink` | `TelemetrySink` | no |
| `Trace` | `SpanSink` | `SpanSink` | no |
| `Ipc<P>` / channel | `Endpoint` (+ `protocol`) | `Endpoint` | depends on peer |
| `Notify` | `Notification` | `Notification` | yes (signal timing) |
| `Clock` | (ambient today → slot after ambient diet) | `ClockNow`/`ClockFreq` | **yes** |
| `Console` | (ambient today → slot) | `ConsoleRead`/`Write` | in: yes / out: no |

The "nondet?" column is what axis 2 (determinism-as-capability) computes from a process's
slot set: an empty-of-nondet-slots manifest ⇒ a pure process. The manifest's slot list
*is* `main`'s effect row — the purity bit falls out of the same vocabulary.

## Versioning & durability (Q2)

Manifests persist — in ELF notes and FS xattrs — so they have the same
durability exposure the wire format does, and the same currently-unguarded discipline
(append-only-by-comment, roundtrip-only tests). Before manifests are load-bearing for
checkpoint/replay:

- **TLV tags append-only** for Slot fields (so the deferred `protocol`/`optional`/
  `constraints` slot in without breaking old readers).
- **A version byte in the manifest header**, next to the existing 4-byte length prefix.
- **A golden-bytes snapshot test** (`insta`): encode one exemplar of every Slot shape and
  snapshot the exact bytes, so a field reorder or tag reuse fails loudly instead of
  silently skewing readers that didn't rebuild.

Same policy as the wire-hardening hedge in Q2 — the two are the same problem one layer
apart.

---

## Increment plan (thin)

1. **`hitch`: `uses: Vec<String>` → `needs: Vec<Slot>`** with `Slot { name, object,
   rights }` (rich fields encoded-but-unused). Add the header version byte + golden-bytes
   test in the same change. Host-tested.
2. **`#[entry(needs = [...])]`** emits the typed slots into `.snitch.iface` *and* the
   compile-time `SLOTS` name→index const the runtime resolves against.
3. **Runtime `bootstrap().get::<T>(name)`** resolving the role name against `SLOTS` →
   `delegated_handle(index)`. Keep `telemetry()`/`tracer()` (the undeclared bootstrap
   pair) working.
4. **A `satisfy` library** (used by `init`, later the shell + supervisor): read a child's
   manifest, match slots to own caps (mint attenuated where needed), return the ordered
   handle array, emit named grant records. All-or-nothing.
5. **Migrate one consumer end-to-end** — `fs-client` reads its endpoint via
   `bootstrap().get::<Endpoint>("fs")` instead of `delegated_handle(0)`; `init` satisfies
   it via the library. Itest asserts the named grant record on the wire and that the
   client still reaches the FS.
6. **Deferred**: `protocol` schemas + structural compat check at satisfy time; `optional`
   slots + null-sentinel alignment; `constraints` (budgets/IFC); the explicit BootInfo
   page (option A). Each lands when its axis demands it.

## Open questions (the ⚖ decisions)

- **(a) Local-role naming** over any global service namespace — confirm. (Recommended;
  it's what makes interposition/sandboxing free.)
- **(b) Grant record** = satisfier-emitted telemetry (recommended) vs a new kernel frame.
- **(c) Optional slots** in the thin build, or is everything required to start? (Lean:
  all-required; optional waits for a program that genuinely degrades.)
- **(d) Delivery**: manifest-as-index (recommended thin) vs explicit BootInfo page. The
  page is strictly more flexible but needs a kernel copy-in; the index needs nothing.
- **(e) Where `ObjectKind`/`Rights` live** so all three schemas share them — `abi`
  already defines `object_kind` + `rights`; the Slot should reference those, not fork a
  parallel enum (the "rights namespaces about to multiply" concern, Q3 #7).
