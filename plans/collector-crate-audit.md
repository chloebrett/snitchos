# Crate audit — `collector/`

_Audited 2026-06-08. Evidence-backed._

> **Applied 2026-06-08:** #6 (removed unused `postcard` dep), #1 + #2 (fixed the
> two stale doc comments), #3 (**deleted** `timebase_hz()` accessor + its two
> accessor-only tests — the `self.timebase_hz` field stays, exercised via
> `tick_to_wall_ns`), #5 (silenced `match_same_arms` with a documented intent
> note), #8 (dropped needless `pub` on `otlp::Exporter::export`). Verified:
> `cargo test -p collector` → 43 pass, **clippy fully clean** (the one firing
> warning is gone).
>
> **Applied 2026-06-08 (round 2):** #4 — extracted `url::ensure_suffix` into a new
> `collector/src/url.rs` (not a generic `util.rs`); both exporters delegate; the
> two redundant double-append tests collapsed into three focused `url` tests + a
> per-exporter wiring guard. #7 — dropped `fastrand`; the OTLP `trace_id` now
> derives from session start-time nanos (`session_trace_id`), since per-run
> uniqueness — not entropy — is all it needs. Verified: 44 pass, clippy clean,
> no `fastrand` left in the tree. **All collector findings now resolved.**

## Scope

- **Target:** `collector/` — `main.rs` (135) · `state.rs` (639) · `otlp.rs` (272) ·
  `prom.rs` (206) · `loki.rs` (169). 1421 lines total.
- **Mandate:** host-side daemon. Connect to the kernel telemetry socket, decode
  `protocol::Frame`s, maintain span/string/metric state, and fan out to sinks:
  stdout (`--text`), OTLP/HTTP traces (Tempo), Loki logs, Prometheus `/metrics`.
- **Publish status:** `publish = false`, **bin-only** (no `lib.rs`, nothing depends
  on it). **Decisive scoping fact:** there is no external API surface at all — every
  `pub` item is internal-only, so "no in-repo caller == dead." The "might be public
  API" escape hatch does not exist here. (This directly contradicts an in-code
  comment — see #3.)
- **Health:** builds clean (no `dead_code`/`unused` warnings), but clippy pedantic
  (warn-enforced via `[lints] workspace = true`) **fires one warning today** (#5),
  and several doc comments have gone stale against the code (#1, #2).

## Findings

| # | Dim | Sev | Finding | Evidence | Recommendation | Effort | Risk |
|---|-----|-----|---------|----------|----------------|--------|------|
| 1 | G | **med** | Module doc lies about current state: "v0.2 scope: `--text` works; `--otlp` and `--prometheus` are **stubs** that print 'not yet implemented'." All three are fully implemented (272-line OTLP exporter, 206-line prom server), and a 4th sink (Loki) exists but isn't mentioned. | `main.rs:5-6` vs `otlp.rs`, `prom.rs`, `loki.rs`. | Rewrite the doc to describe the four live sinks. Pure comment fix. | XS | none |
| 2 | G | **med** | Stale `Args` doc: "Prometheus exposition is **off by default** until v0.2 step 7 is implemented." It's **on** by default — `prometheus` defaults to `9091` and `prom::serve` runs unless `--no-prometheus`. | `main.rs:33-34` vs `main.rs:66-67,87-89`. | Fix the doc to say on-by-default, disabled via `--no-prometheus`. | XS | none |
| 3 | A | **med** | `State::timebase_hz()` is `pub` with **zero production callers** — used only by two tests that exist solely to exercise the accessor. Its `#[allow(dead_code)]` justification cites a "lib build" and "consumers" that **don't exist** (bin-only crate). | `state.rs:283-289`; callers: 0 in `main/prom/otlp/loki`, 2 in `state.rs` tests (`:473,:479`). | Scope the accessor `#[cfg(test)]` (drops the misleading `allow` *and* the dead-code question), **or** delete it + its two tests (the field `self.timebase_hz` is independently exercised via `tick_to_wall_ns` tests). Your call on which. The wrong-justification `allow` should go either way. | XS | none (no external API) |
| 4 | D | med | URL-suffix normalization is duplicated verbatim across the two HTTP exporters: the idempotent "append path unless already present" shape, **plus a near-identical pair of tests each**. | `loki.rs:20-24` ‖ `otlp.rs:119-123`; tests `loki.rs:159,165` ‖ `otlp.rs:262,268`. | Extract `fn ensure_suffix(base: &str, suffix: &str) -> String` (shared util) + one focused test for it; each exporter passes its own suffix. Collapses ~10 prod lines and 2 redundant tests. Same contract, genuinely collapsible. | S | low |
| 5 | A′ | low-med | `clippy::match_same_arms` **fires today** (pedantic = warn, enforced): `Frame::Event => None` and `Frame::Dropped => None` have identical bodies. But the comments mark different intent ("not yet wired to OTLP" vs nothing-to-export). | `state.rs:241,258`; `cargo clippy -p collector` → 1 warning. | Keep them separate (the intent differs) but silence honestly: `#[allow(clippy::match_same_arms, reason = "Event is parked pending OTLP wiring; Dropped genuinely has nothing to export")]`. Merging would erase the distinction. | XS | none |
| 6 | H | **med** | `postcard` is a declared dependency with **zero direct uses** in the crate — all decoding goes through `protocol::stream`, which owns its own postcard dep. | `grep postcard collector/src` → 0; `Cargo.toml:13`. | Remove `postcard = "1"` from `collector/Cargo.toml`. Clear win. | XS | none (build will fail loudly if wrong) |
| 7 | H | low | `fastrand` (whole dep) is used for exactly one call — 16 random bytes for a per-session `trace_id`. | `otlp.rs:235` (sole site); `Cargo.toml:16`. | Defensible (trace IDs want entropy) — but replaceable with a few lines of std if you want one fewer dep. Your call; lean keep. | S | low |
| 8 | F | low | `otlp::Exporter::export` is an inherent `pub fn` reached only via the trait forward (`SpanExporter::export → self.export`); `main` calls through the trait object. The `pub` is unnecessary. | `otlp.rs:133` inherent; `otlp.rs:227` forward; `main.rs:115` trait call. | Drop `pub` (→ `fn export`). The inherent/trait split itself is intentional (`mutants::skip` granularity, doc'd at `otlp.rs:224`) — keep that. | XS | none |

## Two lists

**Obvious wins (safe now, low risk):**
- **#6** — remove the unused `postcard` dependency.
- **#1, #2** — fix the two stale doc comments (module scope + Prometheus default).
- **#3** — `#[cfg(test)]` the `timebase_hz` accessor and drop the false `allow` reason.
- **#5** — replace the firing `match_same_arms` warning with an `allow` + reason.
- **#8** — drop the needless `pub` on `otlp::Exporter::export`.

Mass: ~1 dep line + a handful of doc/annotation edits; near-zero prod logic change.

**Needs your call:**
- **#3 variant** — `#[cfg(test)]` the accessor vs delete it + its two tests outright.
- **#4** — extract the shared `ensure_suffix` URL helper (collapses ~10 lines + 2 tests),
  or leave the exporters independently readable. Mild judgement call.
- **#7** — keep `fastrand` for trace-id entropy, or drop it for a std-only generator.

## Verdict

`collector/` is in good shape — well-tested (≈300 of 639 lines in `state.rs` are
tests; exporters and prom each carry focused tests), no dead private logic, clean
builds. The debt is almost entirely **documentation drift**: comments written when
OTLP/Prometheus were stubs (#1, #2) and a dead-code `allow` whose rationale assumes
a library crate this isn't (#3). One unused dependency (#6) and one firing lint (#5)
round it out. All obvious wins are doc/annotation/manifest edits — no behavior
change, ~15 minutes. The only genuine code question is the #4 exporter URL dedup.
