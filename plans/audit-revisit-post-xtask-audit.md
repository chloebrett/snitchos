# Re-audit with `cargo xtask audit` — itest-harness / protocol / collector (2026-06-08)

Re-ran the three previously-hand-audited crates through the new tool. All three
are `publish = false`. **The decisive scoping fact is crate type** — `ext` is
only a deadness signal for a *library with sibling consumers*; for a binary it's
always 0 and means nothing.

| Crate | Type | Consumer | Verdict |
|---|---|---|---|
| protocol | lib | kernel, collector, xtask | **Clean.** Leave it. |
| collector | **bin** | nobody | Clean of dead code; `pub` is cosmetic. |
| itest-harness | lib | **xtask only** | **Over-exposed API** — the one real finding. |

## protocol — clean, nothing to do

Every `pub` item has `ext > 0`; no candidates, no unused deps, no markers. It's
the kernel↔host wire contract — broad `pub` is the point (rule 6). `Frame`
ext=136, `OwnedFrame` 106, `StringId` 81. Don't touch.

## collector — bin crate, `ext` is noise

`collector` is `main.rs`-only (no lib target); nothing imports it, so **every**
symbol shows `ext=0`. That is not a deadness signal — the candidates section
correctly reports "none" (no symbol is all-zero; each has `int ≥ 1`). No unused
deps. One marker, a *justified* `#[allow(clippy::match_same_arms, reason=...)]`
at `state.rs:254` — keep.
- Low-value option only: several `pub` (e.g. `SystemWallClock`, `serve`,
  `WallClock`, `SpanExporter`, `HISTOGRAM_BOUNDS`) could be `pub(crate)` since a
  bin exposes nothing. Cosmetic; not worth a churn PR.

## itest-harness — DONE: surface 99 → 56 pub items

Sole consumer is `xtask`. 55 of 99 `pub` items showed `ext=0`. Ran the
compiler-backed privatization sweep: demoted all 55 to `pub(crate)`, rebuilt
`itest-harness + xtask`, re-promoted exactly what the compiler proved was
cross-crate. **Result: 43 demoted, 12 re-promoted; public surface 99 → 56, zero
deletions.** All 150 itest-harness + 36 xtask tests green, clippy clean.

The 12 re-promotions were all **tool false positives** — two classes the
word-boundary heuristic structurally can't see:

1. **Re-export alias** (1): `push_with_timeout` is consumed by xtask as
   `push_otlp_with_timeout` (`pub use otlp::push_with_timeout as …` in lib.rs).
   The tool counts by declared name, so the aliased use is invisible → `ext=0`.
2. **Type used positionally in a public signature** (11): `BaselineError`,
   `ScenarioBaseline`, `Signature`, `PruneReport`, `RecoveredRun`,
   `RecoveredScenario`, `FailureEvidence`, `Baseline`, `RunMetadata`,
   `RunMetadataInner`, `PartialMarker`. xtask calls e.g. `BaselineFile::load_path`
   (returns `Result<_, BaselineError>`) or reads `prune_runs`'s `PruneReport`
   without ever *naming* the type → `ext=0`. The compiler's `private_interfaces`
   lint + "type is private" errors surfaced the whole transitive closure (a pub
   field's type must be ≥ as visible as the field), re-promoted in 4 rebuild
   rounds until it converged.

- **Marker false positive** (unchanged): `baseline.rs` `"# stub\n"` literal —
  not a real stub.

## Net

protocol/collector: no action. itest-harness: **done** — 43 `pub`→`pub(crate)`,
zero deletions, surface nearly halved. No unused deps anywhere (contrast
`kernel-core`'s `spin`, `plans/kernel-core-audit.md` finding 0).

## Lesson for the tool / skill

The sweep confirmed two structural false-positive classes in `xtask audit`'s
`ext` count — re-export aliases and types-used-positionally-in-public-signatures.
Both mean: **`ext=0` is a candidate, and the compiler is the oracle.** The
privatization sweep (demote-all-then-let-the-compiler-re-promote) is the correct
procedure precisely because it turns those blind spots into build errors. Noted
in `plans/xtask-audit.md` and the `crate-audit` skill.
