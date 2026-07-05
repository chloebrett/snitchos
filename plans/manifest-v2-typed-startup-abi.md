# Plan: Manifest v2 — typed, named startup ABI (redesign #2, thin build)

**Branch**: feat/manifest-v2-thin
**Status**: Planned — not started.
**Design**: [docs/manifest-design.md](../docs/manifest-design.md). This plan builds
the **thin** version only (coda Tier-3 #4): `Slot { name, object, rights }`,
delivery by *manifest-as-index* (no new kernel mechanism), a userspace satisfier.
The rich fields (`protocol`, `optional`, `constraints`) and the explicit `BootInfo`
page are **deferred** — encoded-but-unwired so they slot in additively later.

## Goal

Kill the positional `delegated_handle(i)` startup contract: a program **declares**
its authority needs as typed slots and **reads** each granted capability by the role
name it declared, while a satisfier (init) grants them from its own caps by matching
the manifest — the kernel `Spawn` ABI unchanged and name-blind throughout.

## The design in one paragraph

Promote hitch's `Manifest.uses: Vec<String>` (bare strings, drives no grant) to
`needs: Vec<Slot>` where `Slot = { name, object, rights }`. A child carries its own
manifest in its `.snitch.iface` note, so it knows its slot names *and their order*;
the `#[entry]` macro also emits a compile-time `SLOTS` name→index table. The runtime
resolves `bootstrap().get::<Endpoint>("fs")` → `slot index` → `delegated_handle(index)`
— so slot 0 lands at the same handle 2 init already delegates. That's why the
**child side migrates first on today's delegation** (Increments 1–4), and the generic
**satisfier** (Increments 5–6) — read a child's manifest, match slots to own caps
all-or-nothing, mint attenuated where a slot asks for less, emit named grant records
— lands after, without a flag day.

## Assumptions (the design's ⚖ decisions, resolved with the doc's leans)

These are taken as decided for this plan; flag if any should change before the
relevant increment:

- **(d) Delivery = manifest-as-index**, not the explicit `BootInfo` page. No kernel
  change; the positional order *is* the manifest slot order.
- **(e) `Slot.object`/`.rights` mirror `abi::object_kind` / `abi::rights`** as raw
  discriminants (`u8`/`u32`), not a forked enum — same pattern as
  `protocol::CapObject` mirroring `abi::object_kind`, with a test asserting agreement.
  Avoids a hitch→abi hard dependency and keeps the wire encoding trivial.
- **(b) Grant records are satisfier-emitted telemetry**, not a new kernel frame.
- **(c) All slots required** in the thin build (`optional` deferred).

## Decisions (confirmed 2026-07-05)

- **Increment 1 — `Slot.object`/`.rights` mirror `abi` discriminants** (raw
  `u8`/`u32`), not a forked enum or a hitch→abi hard dep. Locked.
- **Increment 6 — the satisfier reads the child's manifest from the FS `user.iface`
  xattr, and launches via `SpawnImage`.** The mechanism already exists
  (`workload=manifest-iface` reads exactly this xattr). So the first generic-satisfier
  demo runs the FS/`SpawnImage` path — init reads the child ELF + its lifted manifest
  off the seeded FS, satisfies, and `SpawnImage`s. A kernel "manifest of spawnable id"
  read is **not** pursued. Increments 1–5 don't depend on this.

## TDD discipline (per project rules)

Each increment is **RED first, in its own edit** (failing test), then minimum GREEN,
then **MUTATE** (`cargo xtask mutants` on the touched host module) and kill survivors,
then assess refactor. Never batch test + impl. Host-testable logic lands in
host-buildable crates (`hitch`, proc-macro unit tests, the pure satisfy core) covered
by `cargo test`; riscv userspace wiring is covered by the QEMU `itest`. Commit gate:
`cargo xtask itest --repeat 10` on any increment that touches an itest.

---

## Increment 1 — `hitch::Slot` + typed `needs` + versioned encoding (host-tested)

Replace `uses` with `needs: Vec<Slot>` (`ConstManifest.needs: &'static [Slot]`);
`Slot { name: Str, object: u8, rights: u32 }`. Add a **manifest header version byte**
next to the existing 4-byte length prefix, TLV-encode `Slot` fields **append-only**,
and a **golden-bytes `insta` snapshot** so a field reorder / tag reuse fails loudly.
Coupled in the same PR (they share the emitted bytes): the `#[entry]` manifest
emission, `manifest_demo`, and the `manifest-iface` itest assertion.

**Acceptance criteria**: a `Manifest` with one `needs` slot encodes and decodes with
its `{name, object, rights}` intact; the encoded bytes are golden-snapshotted; the
header carries a version byte; `Slot.object`/`.rights` discriminants match
`abi::object_kind`/`abi::rights`.
**RED** (host, `hitch`): (1) `needs_slot_roundtrips` — encode a manifest with
`Slot{"fs", Endpoint, SEND}`, decode, assert the slot survives; (2)
`slot_discriminants_match_abi` — `Slot.object` for Endpoint == `abi::object_kind::ENDPOINT`, rights bits == `abi::rights::SEND`; (3) `manifest_header_carries_version`;
(4) golden-bytes `insta` snapshot of a canonical manifest.
**GREEN**: the `Slot` type + TLV encode/decode + version byte.
**MUTATE**: `mutants` on the hitch encode/decode — the length prefix, the version
byte, the per-field TLV tags, and the discriminant mirroring are load-bearing.
**Committable slices**: 1a hitch type+encode+tests (host-green alone); 1b macro +
`manifest_demo` + `manifest-iface` itest updated to `needs`.
**Done when**: `cargo test -p hitch` green incl. golden snapshot; `manifest-iface`
itest green on the new shape; mutation report reviewed.

## Increment 2 — `#[entry]` emits the runtime `SLOTS` name→index table

In addition to the `.snitch.iface` note, the macro emits
`static __SNITCH_SLOTS: &[(&str, /*object*/ u8)]` in declaration order — the lean
table the runtime resolves role names against (the note carries the full slots for
satisfiers; the runtime only needs name→index + object-kind for the type check).

**Acceptance criteria**: `#[entry(needs = [Slot{name:"fs", ...}, Slot{name:"log", ...}])]`
expands to a slots table listing `"fs"` at index 0 and `"log"` at index 1.
**RED** (host, proc-macro unit test, like #4's root-span test):
`emits_slots_table_in_declaration_order` — assert the expansion contains the table
with names in order; `no_needs_emits_empty_table`.
**GREEN**: emit the const.
**MUTATE**: proc-macro logic is thin; confirm the ordering + empty-case tests cover it.
**Done when**: `cargo test -p snitchos-user-macros` green; a bin still builds/embeds.

## Increment 3 — pure `slot_index` + typed `bootstrap().get` (host core + runtime)

The name→index lookup and the object-kind type check are pure — put them in `hitch`
(host-tested); the runtime's `bootstrap().get::<T>(name)` wraps them over `SLOTS` +
`delegated_handle`.

**Acceptance criteria**: `get::<Endpoint>("fs")` on a manifest declaring `fs` at
index 0 resolves to `delegated_handle(0)`; `get` for an undeclared name → `None`;
`get::<Endpoint>` on a slot declared `Notification` → type-mismatch error (not a
wrong-typed handle).
**RED** (host, `hitch`): (1) `slot_index_finds_declared_name`; (2)
`slot_index_missing_name_is_none`; (3) `get_type_must_match_declared_object`.
**GREEN**: `slot_index(slots, name) -> Option<usize>` + an object-kind guard; runtime
`bootstrap()`/`get` wrapper (riscv, no host test — covered by Increment 4's itest).
**MUTATE**: `mutants` on `slot_index` + the type-guard — the equality checks are the
load-bearing lines.
**Done when**: `cargo test -p hitch` green; runtime compiles for riscv.

## Increment 4 — migrate `fs-client` to read by name (child side, on today's delegation)

`fs-client` declares `#[entry(needs = [Slot{name:"fs", object:Endpoint, rights:SEND}])]`
and reads its endpoint via `bootstrap().get::<Endpoint>("fs")` instead of
`Endpoint::from_raw_handle(delegated_handle(0))`. **Works against init's existing
positional delegation** (slot 0 → handle 2), so no satisfier change yet — this is the
"kill the magic index *in the program*" milestone.

**Acceptance criteria**: `fs-client` contains no `delegated_handle(` literal; the
`fs` workload's existing assertions still pass (client reaches the FS end to end).
**RED**: the `fs`-family itests (`fs-connect-mints-root` … `fs-remove`) with
`fs-client` migrated — RED until `bootstrap().get` is wired (Increment 3).
**GREEN**: the migration + Increment 3's runtime wrapper.
**MUTATE**: n/a (riscv wiring; covered by itest). Confirm via `--repeat 10`.
**Done when**: `cargo xtask itest --repeat 10` green on the `fs` family; `grep
delegated_handle user/fs/src/bin/fs-client.rs` empty.

## Increment 5 — the pure `satisfy` matching core (host-tested)

A pure function over abstract cap descriptors: given a child's `needs: &[Slot]` and
the satisfier's own caps `&[CapView{object, rights, handle}]`, produce an ordered
per-slot plan (`Grant{handle}` or `Grant::MintAttenuated{from, rights}`) or
`Unsatisfied{slot}`. All-or-nothing. Lives in a `no_std` host-testable module
(hitch or a new `manifest-satisfy`; decide at Increment 5a).

**Acceptance criteria**: exact-match → the handle; a required slot with no matching
cap → `Unsatisfied`; a slot asking fewer rights than a matching cap → `MintAttenuated`
with the slot's rights; two slots → plan in slot order; object-kind mismatch → not a
match.
**RED** (host): `exact_match_grants_handle`; `unmatched_required_slot_is_unsatisfied`;
`narrower_rights_plan_mint_attenuated`; `plan_is_in_slot_order`;
`object_kind_mismatch_is_not_a_match`.
**GREEN**: the matcher.
**MUTATE**: `mutants` — the rights-subset check, the object-kind equality, and the
all-or-nothing early-out are load-bearing.
**Done when**: `cargo test` green on the satisfy module; mutation report reviewed.

## Increment 6 — generic satisfier in `init` + named grant records (integration)

`init` reads a child's manifest **from the FS `user.iface` xattr and launches via
`SpawnImage`** (decided above), runs the Increment-5 matcher, mints
attenuated caps via `MintBadged`, assembles the handle array in slot order, `Spawn`s,
and emits one **named grant record** per satisfied slot as telemetry (`granted fs ⟵
cap <cap_id>`, off the v0.13 cap-id spine).

**Acceptance criteria**: for the migrated child, a grant-record telemetry frame naming
the slot (`"fs"`) and the granted `cap_id` appears on the wire; the child still
reaches the FS; an **unsatisfiable** required slot refuses the spawn (snitched), not a
partial grant.
**RED** (itest, new `workload=`): `manifest-satisfy-grants-by-name` — assert the named
grant record + end-to-end reach; `manifest-satisfy-refuses-unsatisfiable` — a child
declaring a slot init can't satisfy → refused spawn on the wire.
**GREEN**: the satisfier wiring in init + grant-record emission.
**MUTATE**: n/a (riscv); the matcher's mutation coverage is Increment 5. `--repeat 10`.
**Done when**: both new itests green on `--repeat 10`; grant record visible in Grafana.

---

## Deferred (rich vN — explicitly out of this plan)

- `Slot.protocol` (`TypeSchema`) + structural compat check at satisfy time.
- `Slot.optional` + null-sentinel handle alignment for unsatisfied optionals.
- `Slot.constraints` (budget size / IFC clearance) — plugs into the extension point.
- The explicit `BootInfo` page (delivery option A).
- Kernel "manifest of spawnable id" read (if registry-spawn satisfaction is wanted
  over the FS/`SpawnImage` path).
- Migrating the *other* consumers (shell `~>`, supervision, checkpoint, Stitch `uses`)
  — each is its own downstream plan once this lands.

## Pre-PR quality gate (each increment)

1. Mutation testing on the touched host module (`cargo xtask mutants`).
2. Refactoring assessment (`refactoring` skill) — only if it adds value.
3. `cargo xtask clippy` clean; `cargo xtask itest --repeat 10` if an itest changed.
4. Wire-durability check: any change to the manifest encoding re-snapshots the
   golden-bytes test intentionally (a diff there must be a deliberate, reviewed byte
   change — the whole point of Increment 1's guard).

---
*Delete this file when the plan is complete. If `plans/` is empty, delete the directory.*
