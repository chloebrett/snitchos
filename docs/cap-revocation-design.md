# Capability revocation — design

**Status: SHIPPED (2026-06-28) — built as designed, and transitive from day one.**
`Syscall::Revoke = 28` takes a *handle* (holding the cap **is** the authority to
reclaim its derivations); `sched::revoke_descendants_of` is the cross-table fixpoint;
each swept holding emits a `CapEvent::Revoked`. Userspace binding:
`Endpoint::revoke_derived`. Itest: `revoke-reclaims-a-minted-cap`. The Stitch shell's
`hold`/`grant`/`revoke` verbs landed too, closing grant → use → reclaim end to end.
Deferred, as scoped below: object-level revoke and `DropCap`.

Originally captured 2026-06-28 as design / exploration.
Prework for the **Stitch shell** (decided: the shell is Stitch). The shell is a
powerbox — *"grant, then watch"* ([shell-surface-and-tui-design.md],
[userland-text-streams-and-the-actor-model-design.md]) — and revocation completes
its lifecycle into **grant → use → reclaim**, with the reclaim *also* an observable
`CapEvent`: "watch authority get taken back."

Builds on the shipped v0.13 **cap-id spine** (every holding carries a global
`cap_id`; a transfer records `parent_cap_id`; `CapEvent::{Granted,Transferred}`
frames reconstruct the derivation tree host-side) and the existing per-slot
generation machinery.

## What already exists

- **Per-slot invalidation.** `CapTable::consume(handle)` frees a slot and bumps its
  `generation`, so the handle (and any copy *in the same table*) then resolves
  `CapError::Stale`. Reserved as "the consume step of a single-use capability — and
  the long-reserved revocation path." This is the building block.
- **`Handle` = slot index + generation** (packed `u32`); `resolve` checks the
  generation, so a bumped generation invalidates stale handles to that slot.
- **`cap_id` per holding** (`Slot.cap_id`, minted kernel-side via `next_cap_id`,
  set at every grant/transfer). The derivation-tree *node identity*.
- **`CapEvent` wire frames** carry `cap_id` + `parent_cap_id`; `CapEventKind` =
  `{Granted, Transferred}` today. No `Revoked` yet.

## The crux: kernel doesn't store the derivation tree

`parent_cap_id` is emitted on the `CapEvent::Transferred` **frame** but is **not
stored on the `Slot`** — the kernel keeps only each holding's own `cap_id`. The
tree is reconstructed by the *collector*, host-side. So today the kernel can find
"the holding(s) with `cap_id == X`" (by scanning tables) but **cannot walk
descendants** — it has no parent links. Kernel-*enforced* transitive revocation
therefore requires adding `parent_cap_id` to `Slot` (a `u64`/slot). Small, but it's
the enabler, and it must be decided up front.

A second structural fact: there is **no global `cap_id → (process, slot)` index**.
Finding a holding by `cap_id` means scanning every live process's `CapTable`
(`SCHEDULER.tasks` → each `Process.caps`). O(processes × slots) — fine at today's
scale (single-digit processes, tiny tables), but note it; an index is the
optimization if it ever bites.

## Three levels of "revoke"

| Level | What it does | Cost | Covers |
|---|---|---|---|
| **0. Exit-reclaim** *(already works)* | program exits → `reap_task` drops its `CapTable` → its caps vanish | free | implicit reclaim on child death |
| **1. Drop-own-holding** | expose `consume` as a "drop *this* handle" syscall | trivial | the init-over-holds `RECV` cleanup; a process shedding its own authority |
| **2. Revoke-a-grant (by `cap_id`)** | invalidate a holding I granted, *in another process's table*, by `cap_id` | medium | the powerbox **reclaim** — shell takes back what it gave |
| **2T. + transitive** | also invalidate descendants (grantee re-delegated onward) | medium-high (needs `Slot.parent_cap_id`) | airtight reclaim regardless of re-delegation |

Level 0 + 1 are not the powerbox feature. **Level 2 is the shell's "reclaim."**
Level 2T is 2 made airtight against onward re-delegation.

## Open design questions (decide before building)

1. **Transitive or not (2 vs 2T)?** Non-transitive (2) is simpler and correct *if*
   grantees don't re-delegate, or if we accept that a re-delegated copy survives a
   parent revoke (and document it). Transitive (2T) is airtight but needs
   `Slot.parent_cap_id` + a descendant walk. **Lean: ship 2 first, with
   `Slot.parent_cap_id` added so 2T is a later additive step — not a re-design.**
2. **Who may revoke `cap_id` X?** Candidate rule: a process may revoke X iff X is a
   *descendant of a holding it currently holds* (you can reclaim only what you
   granted, directly or transitively). Verifiable once `parent_cap_id` is stored.
   Simpler interim: you may revoke X iff you hold the *immediate parent* holding.
   (Reject "anyone may revoke any cap_id" — that's an authority hole.)
3. **Holding-level vs object-level.** Revoke *a holding* (this grant) vs revoke *the
   underlying `Object`* (every holding of this endpoint/notification, everywhere).
   The shell wants holding-level (reclaim *my* grant). Object-level is a different
   feature (destroy an endpoint) — out of scope here; note it.
4. **What does the victim observe?** Its next syscall on the revoked handle resolves
   `Stale` → the existing `SyscallRefused` snitch (counter + frame). No async
   notification to the victim (it finds out by trying to use it). Confirm that's
   acceptable for v1 (it matches the "refusals snitch, never silent" ethos).
5. **`CapEvent::Revoked`** — add the variant (carrying `cap_id` + `holder`), emit on
   revoke. The wire-format rule: *append* the variant (postcard is positional —
   don't reorder), update `OwnedFrame::from_borrowed`. Collector marks the node
   revoked in its derivation tree (a later, additive collector change).
6. **Interaction with one-shot / reply caps.** `consume` is already the one-shot
   path; revoke must not double-free a slot a `reply` already consumed (generation
   check handles this — a stale `cap_id` match is skipped). Verify in tests.
7. **Self-revoke vs grant cleanup.** Level 1 (drop-own) is arguably a separate small
   syscall (`DropCap(handle)`); decide whether to ship it alongside or fold "revoke
   a cap_id I hold directly" into the Level 2 path.

## Recommended shape (for discussion)

- **Kernel-core (pure, host-tested):** add `Slot.parent_cap_id`; a
  `CapTable::revoke_by_cap_id(cap_id) -> RevokeOutcome` that invalidates a matching
  *live* holding (free slot + bump generation, like `consume`) and reports what it
  did; an ancestry check `is_descendant(cap_id, ancestors)` for the who-may-revoke
  rule. (2T adds a descendant walk across the table.) All host-testable against a
  `CapTable` with synthetic cap_ids — no kernel needed.
- **Kernel side:** a `Revoke(cap_id)` syscall (cap-mediated: the authority check is
  ancestry, not a held handle to the victim slot); scan live processes' tables;
  emit `CapEvent::Revoked`. Refusals snitch.
- **Shell use case it unlocks:** `grant(file, view) ~> run ~> revoke` — the shell
  delegates a file cap to a spawned `view`, and reclaims it when the command ends;
  three `CapEvent`s on the wire (Granted/Transferred, then Revoked). The
  observable "least-authority, with a clock on it."

## Build order (TDD, additive on shipped mechanism)

1. **kernel-core:** `Slot.parent_cap_id` + `revoke_by_cap_id` (non-transitive, Level
   2) + ancestry helper. Host tests: revoke invalidates the holding; stale/again is
   a no-op; generation bump makes copies-in-table stale.
   - ✅ **2T prework landed (2026-06-28):** `Slot.parent_cap_id` stored at every
     grant (threaded through `grant`/`insert_with_id`/`insert_once_with_id`;
     `parent_cap_id_of` reader); the five kernel grant sites now persist the same
     `parent_cap_id` they emit on the wire (transfers: MintBadged, run_with_caps
     delegation; roots → 0: bootstrap, EndpointCreate, NotifyCreate, run_ipc). The
     kernel-side derivation tree is now walkable — the enabler for transitive (2T)
     revocation. kernel-core 434 green; `spawn-transfer-links-to-parent` still green.
   - ✅ **`revoke_by_cap_id` landed (2026-06-28):** per-table primitive — frees the
     live slot whose `cap_id` matches + bumps generation (→ `Stale`), non-transitive,
     `cap_id == 0` (root sentinel) is a no-op. 3 host tests (invalidates exactly that
     holding; no-op for absent/already-revoked; refuses 0). kernel-core 437 green.
   - ✅ **`children_cap_ids` landed (2026-06-28):** per-table helper returning the
     `cap_id`s of live holdings whose `parent_cap_id` matches — the 2T frontier
     expander; root sentinel `0` → empty (never sweep the forest). 2 host tests
     (direct children only; excludes revoked + the `0` sentinel). kernel-core 439 green.
   - The 2T walk itself (cross-table fixpoint over `children_cap_ids` + per-table
     `revoke_by_cap_id`) lives kernel-side in the `Revoke` syscall step.
2. ✅ **protocol `CapEventKind::Revoked` landed (2026-06-28):** appended after
   `Transferred` (positional discriminant = 2); carries the standard `CapEvent`
   fields (`cap_id` = revoked holding, `holder` = process it was taken from). No
   `OwnedFrame` arm needed (kind passes through). Postcard encode→decode roundtrip
   test locks the discriminant. protocol 38 green; collector + itest-harness build.
   (Kernel build is currently blocked by an *unrelated* in-progress `Syscall::CapList`
   gap in the dispatch match — the `hold` work — to be fixed separately.)
3. ✅ **`Revoke` syscall landed (2026-06-28) — and it's transitive (2T) from day one,
   since the prework was in place.** `Syscall::Revoke = 28` takes a **handle** (not a
   raw cap_id): resolving it in the caller's table *is* the authority (holding the
   cap = the right to reclaim what was derived from it; no separate ancestry check).
   `sched::revoke_descendants_of(root_cap_id)` runs the cross-table fixpoint (pop
   node → `children_cap_ids` across every `Process.caps` under the `SCHEDULER` lock →
   `revoke_by_cap_id` each → push back; terminates because child cap_id > parent),
   returning `(holder, cap_id, parent_cap_id, cap)` per revoked holding; the handler
   emits a `CapEvent::Revoked` for each and returns the count. The caller's own
   holding survives. Userspace binding: `Endpoint::revoke_derived()`. itest
   `revoke-reclaims-a-minted-cap` (`ep_maker` mints a badged SEND then revokes it):
   asserts a `Revoked` frame linked to the endpoint + `revoked == 1`; 10/10 on
   `--repeat`. Full lower-stack green (kernel-core 441, protocol 38, abi).
4. ⬜ **(later) shell:** `revoke` verb wired to the syscall; the powerbox demo.

(Note: the planned Level-2-first / 2T-later split collapsed — doing the 2T prework
first meant the syscall got transitivity for free. `DropCap` + object-level revoke
remain deferred until a use case appears.)

## Decision needed

Pick the target level (2 vs 2T vs start-with-1), the who-may-revoke rule, and
whether `DropCap` ships alongside. Recommendation: **build Level 2 non-transitive
now, store `parent_cap_id` so 2T is additive, who-may-revoke = "descendant of a
holding I hold," defer object-level + `DropCap` unless the shell needs them.**

[shell-surface-and-tui-design.md]: shell-surface-and-tui-design.md
[userland-text-streams-and-the-actor-model-design.md]: userland-text-streams-and-the-actor-model-design.md
