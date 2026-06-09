# xtask crate audit (2026-06-09)

**Scope:** `xtask` — the build/test/run orchestrator. Binary crate (`main.rs`,
no lib), `publish = false`, consumed by nobody → every `pub` is internal, so the
tool's `ext` column is **noise here** (same as `collector`). The signal is in
markers, long functions, and architecture. ~8.1k LOC; mass in `scenarios.rs`
(1180, a scenario catalogue — fine), `audit.rs` (791, ~half tests), `itest.rs`
(785), `main.rs` (722), `itest/harness.rs` (606).

**Mechanical pass (`cargo xtask audit xtask`): clean.** No dead candidates (no
all-zero symbol), no unused deps. So nothing to delete. The findings are all
**structural** — the part the tool can't see, surfaced by the mandatory by-hand
architecture pass.

## Findings

| # | Dim | Sev | Finding | Evidence | Recommendation |
|---|-----|-----|---------|----------|----------------|
| 1 | E | **med** | **`Cmd::Itest` is a god-subcommand.** 24 fields; 8 of them are mutually-exclusive *modes* (push-otlp, export-prom, prune-runs, recover-pending, adopt-run, promote/discard-pending, baseline-show) dispatched by an early-`return` precedence ladder before the actual test run. Flags pretending to be subcommands. | `main.rs:417-471` (the 24-field destructure + if-let ladder); `main()` itself trips `too_many_lines` (110) largely from this arm | ✅ **DONE.** Split into `cargo xtask baseline {show,promote,discard,recover,adopt,prune,export,push}` (new `BaselineCmd` subcommand + `baseline()` dispatcher); `Cmd::Itest` slimmed from 24→12 fields (run-only). The if-let precedence ladder is gone; modes are now mutually exclusive *in the type*. Dropped the `@latest` sentinel (positional `Option<PathBuf>` handles "latest"); `push_otlp_metrics` now takes `Option<&str>`. Updated runtime hint strings + README + doc comments. **Module split too:** the 8 verbs + helpers moved to `itest/baseline.rs` (`itest.rs` 785→414, `baseline.rs` 391) and the duplicated load+timestamp+push core dissolved into a shared `baseline::load_and_push` (used by both `push_otlp_metrics` and the run path's `try_auto_push`). Build/clippy clean; run path + auto-push verified intact. |
| 2 | E/F | low→med | **`itest::run` had 10 positional args** behind `#[allow(clippy::too_many_arguments, reason = "…refactor when more land")]`. | `itest.rs:123-134` | ✅ **DONE.** Introduced `pub struct RunConfig` (the 10 flags, owned); `run` now takes one value and destructures it. The `#[allow]` is removed (0 left in `itest.rs`). Also fixed a stranded doc-comment bug surfaced en route — `run`'s doc (incl. a stale `keep_existing_qemus` ref) had been merged into `set_capture_level`'s; each now has its own. `main.rs` builds the struct from the clap flags. Build/clippy clean; 36 tests + run/auto-push/skip paths verified. |
| 3 | G | low | **`scan_markers` over-counts in `audit.rs` itself** (8 of 14 marker hits): the `MARKERS` const, the doc comment, and the test fixtures (`"// TODO: revisit"` etc.). Also `main.rs:326` "GDB stub" matches `stub` as a substring. | audit table markers section | Non-debt — a known tool limitation (substring match, self-reference). Optional: have `audit` skip its own `MARKERS`/`source.rs` definitions, or match `// TODO`/`# stub`-at-word-boundary. Low value. |
| 4 | F | low | **~30 scenario fns + the `audit.rs` API are `pub` but internal** (`ext=0`, used once via the `SCENARIOS` registry / by tests). | audit table: `boot_reaches_heartbeat`…, plus `extract_pub_symbols`/`PubSymbol`/`SymbolReport`/`parse_machete` etc. | A `pub`→`pub(crate)` sweep *could* tighten them, but it's a **binary** — `pub` exposes nothing and `dead_code` already covers true deadness. Low value; skip unless doing finding 1 anyway (then tidy the touched modules). |

## Non-findings (checked)

- **`scenarios.rs` at 1180 lines** — a flat catalogue of one `fn` per integration
  scenario, each registered in `SCENARIOS`. That's the intended shape (add a
  scenario = add a fn + a table row), not a god-module. Leave it.
- **`audit.rs`/`source.rs`** (the tool we just built) — clean; ~half is tests.
- **`runner.rs` 277-line fn** — flagged by clippy but lives in `itest-harness`,
  not `xtask`. Out of scope here; note for an itest-harness pass.

## Abstraction opportunity (the mandatory pass)

Finding 1 *is* the warranted abstraction: the baseline/run-management verbs are a
cohesive sub-domain (they all read/write `.itest-baseline.toml` + `.itest-runs/`)
currently smeared across `Cmd::Itest`'s flag space. Lifting them into a `baseline`
subcommand (clap) + their own dispatch is structure that pays — it removes the
precedence ladder, makes mutually-exclusive modes mutually-exclusive *in the type*
(you can't pass `--prune-runs` to a run), and self-documents in `--help`. Benefit:
clarity + the `too_many_arguments` allow disappears. Cost: a clap restructure and
churn in `main.rs` + `itest.rs`'s entry points (the operation fns are untouched).
**Recommend, but ask** — it's a user-facing CLI change (`xtask itest --prune-runs`
becomes `xtask baseline prune`), so it wants your sign-off on the surface.

## Mass estimate

No deletions, no unused deps. Finding 1 is net-neutral-ish (moves code, adds a
subcommand enum) but a real clarity win; finding 2 folds into it. The crate is
healthy — its one wart is a CLI that grew modes as flags.
