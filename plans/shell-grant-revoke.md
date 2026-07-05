# Plan: `grant` / `revoke` verbs for the Stitch shell

**Goal:** give the Stitch shell hands, not just eyes. `hold` (done) shows the caps a
process holds, now legible + color-coded. `grant` and `revoke` *move* authority —
and because each maps to a kernel syscall that emits a `CapEvent`, every grant and
revoke is a span you can watch in Tempo. The "watch least-authority happen" loop:
`hold` → grant a narrow cap → `hold` (see it appear) → `revoke` → `hold` (gone).

## Recon (kernel primitives — all exist)

| Verb | Syscall | Args (a0..) | Returns | Authority |
|---|---|---|---|---|
| `revoke` | `Revoke` (28) | a0 = handle | count of revoked descendants (`usize::MAX` = bad handle) | holding the handle *is* the right |
| `grant`/`mint` | `MintBadged` (11) | a0 = endpoint handle, a1 = badge, a2 = rights | new handle (`usize::MAX` = refused) | must hold `MINT` on the endpoint |
| `hold` | `CapList` (27) | a0 = buf, a1 = cap | live count | ambient (introspection) |

- **Userspace bindings already exist** (`user/runtime/src/lib.rs`): `Endpoint::revoke_derived() -> usize` and `Endpoint::mint_badged(badge, rights) -> Result<usize, Denied>`, plus `Endpoint::from_raw_handle(h)`. So `RuntimePlatform` can build an `Endpoint` from a raw handle and call these — no new ecall wrappers strictly required.
- **Observability is free on-target:** `MintBadged` emits `CapEvent::Transferred`, `Revoke` emits one `CapEvent::Revoked` per swept descendant (`kernel/src/syscall/cap.rs`). The shell writes nothing to the wire; the syscalls do.
- **Design docs agree** (`docs/shell-surface-and-tui-design.md`, `docs/cap-revocation-design.md`): the shell is "a powerbox you can see through"; granting authority is the primary act; revoke authority = holding the handle.

## Design decisions

1. **Gating.** Both verbs are **ungated** in the `uses` sense — like `hold`. Authority is capability-mediated: `revoke` needs you to hold the handle (dropping authority grants nothing); `grant` needs `MINT` on the endpoint, enforced by the *kernel*, not an ambient `uses` tag. No new authority string.
2. **Platform seam** (mirror `hold`). Two new `Platform` methods:
   - `fn revoke(&self, handle: Handle) -> Result<usize, CapError>` — count revoked, or an error for a bad handle.
   - `fn grant(&self, handle: Handle, badge: u64, rights: Rights) -> Result<Handle, CapError>` — the new handle, or refusal.
   - `NullPlatform`: refuse (no caps). `RuntimePlatform`: build `Endpoint::from_raw_handle` + call the binding. `FakePlatform`: mutate an in-memory table (see below).
3. **`FakePlatform.caps` becomes mutable** (`RefCell<Vec<CapInfo>>`), so a host test can `grant`/`revoke` and observe the change through a follow-up `hold`. Fake semantics stay *simple* — remove the handle's entry on revoke (return 1), append a new `CapInfo` on grant (return its handle). Transitive revocation is the kernel's job, covered by the existing kernel revoke itest; the shell test only pins that the verb calls the seam and reports the result.
4. **Natives.** `native_revoke` (arity 1) and `native_grant` (arity 3) in `natives.rs`, registered in `NATIVES`, each calling the platform method and returning a small result value (the count / the new handle) so the REPL prints something meaningful.

## Increments (TDD, one shippable slice each)

1. **`revoke <handle>`** — ✅ **DONE.** `Platform::revoke(handle) -> Option<usize>` (Null/Fake/Runtime/Std/Counting), a free `snitchos_user::revoke(handle)` binding (preserves the bad-handle sentinel, unlike `Endpoint::revoke_derived`), `native_revoke` (ungated, arity 1) registered in `NATIVES`. Returns the descendant count; errors (`None`) on an unheld handle. The holding survives. Host tests: held→`Some(0)` + holding survives, unheld→error, null-backend→error. Mutants 0 missed; on-target builds. The transitive-count is exercised once `grant` (increment 2) can create descendants.
2. **`grant <handle> <badge> <rights>`** — ✅ **DONE.** `Platform::grant(handle, badge, rights) -> Option<Handle>` (all backends), `native_grant` (ungated, arity 3) registered, `platform::parse_rights("SEND RECV")` (names→bitmask, strict). On-target: `Endpoint::from_raw_handle(h).mint_badged(badge, rights)` (`MintBadged`). The `FakePlatform` grew a derivation model (`caps` → `RefCell`, a `parents` edge-map, a monotonic `next_handle`) so transitive `revoke` is faithful — and the **grant→revoke loop now exercises a non-zero reclaim count**. Tests: mint appends a cap; non-MINT refused; unknown rights error; grant→revoke reclaims only the target's descendants (not a sibling's); remint gets a fresh handle. Mutants 0 missed; clippy + on-target clean.
3. **The loop, on the metal** — ✅ **DONE.** itest `stitch-grant-revoke-capevents` (`stitch-fs`): drives `grant(2, 777, "SEND")` then `revoke(2)` at the REPL prompt and asserts `CapEvent::Transferred{Endpoint, badge=777}` then `CapEvent::Revoked{badge=777}` on the wire — the badge ties the revoke to the exact minted cap. Enabler: `STITCH_REPL_IPC` now holds `SEND | MINT` on its fs endpoint (a shell is a delegating authority) — additive, so the other `stitch-fs` scenarios still pass. Green 10/10 under `--repeat`; view/load/cross-pipe regressions all pass.

## Caveats

- **The demo needs a MINT endpoint.** The plain `stitch-repl` process holds only telemetry + span (`EMIT`) — nothing to mint from, no descendants to revoke. `grant`/`revoke` demo under **`stitch-fs`** (or a variant that hands the REPL an endpoint with `MINT`). Host tests are unaffected — `FakePlatform::with_caps` scripts any table.
- **`CapEvent` linkage** (`cap_id`/`parent_cap_id`) is threaded by the kernel automatically; the shell needn't touch it.

## Open decision (needs the human)

What does `grant` *mean* in this first slice?
- **(A) Self-mint** — `grant <handle> <badge> <rights>` derives a narrower badged cap from an endpoint *you* hold, into *your own* table. Fully reachable now (syscall + binding exist), self-contained, observable. The least-authority loop ships immediately.
- **(B) Spawn-first** — build a shell `spawn`/`view` verb first so `grant` can *delegate to a launched program* (the `view foo` powerbox: run a viewer, hand it a read cap). Richer, truer to the vision, but a longer road: needs `Spawn`/`SpawnImage` (15/26) integration + a process-reference model in the shell.

Recommendation: **(A) first** (revoke → self-mint grant → loop), then **(B) `view`** as the next milestone. Incremental, and each slice is demoable.
