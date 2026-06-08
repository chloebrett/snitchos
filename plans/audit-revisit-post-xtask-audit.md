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

## itest-harness — the real finding: ~55% over-exposed surface

Sole consumer is `xtask`. **55 of 99 `pub` items are never named by xtask**
(`ext=0`, `int>0`) — internal-only API exposed as `pub`. Confirmed by spot-check:
`Aggregator`, `wilson_score_95`, `ConfidenceInterval`, `PruneReport`,
`HistoryWriter`, `RunMetadataInner`, `BaselineError`, … all have 0 xtask callers.

- **Not dead** — these are used *within* itest-harness; they're just over-broad
  visibility. No deletion.
- **Recommendation (needs your call):** a `pub` → `pub(crate)` sweep on the
  ext=0 set. The compiler-backed way is the **privatization detector**: demote in
  batches and rebuild — anything that breaks was genuinely cross-crate; anything
  that compiles was internal. Shrinks the API surface ~2× to what its one
  consumer actually needs.
- **Caveat:** itest-harness is a *harness library* — some breadth may be
  deliberate for future reuse / testability. But `publish = false` + single
  consumer weakens that; this is the textbook "internal-only, demote" case.
- **Marker false positive:** `baseline.rs:898 std::fs::write(&pending, "# stub\n")`
  — the literal `# stub` in a test string, not a real stub. Ignore.

## Net

protocol/collector: no action. itest-harness: one optional privatization sweep
(~55 `pub`→`pub(crate)`, zero deletions). No unused deps anywhere (contrast
`kernel-core`'s `spin`, `plans/kernel-core-audit.md` finding 0).
