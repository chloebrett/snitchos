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
| 1 | E | **med** | **`Cmd::Itest` is a god-subcommand.** 24 fields; 8 of them are mutually-exclusive *modes* (push-otlp, export-prom, prune-runs, recover-pending, adopt-run, promote/discard-pending, baseline-show) dispatched by an early-`return` precedence ladder before the actual test run. Flags pretending to be subcommands. | `main.rs:417-471` (the 24-field destructure + if-let ladder); `main()` itself trips `too_many_lines` (110) largely from this arm | Split into clap **subcommands**: keep `xtask itest [scenario] [run-opts]` for *running*, move the baseline/run-management verbs under `xtask baseline {show,promote,discard,recover,adopt,prune,export,push}`. Shrinks the variant, deletes the precedence ladder, makes the CLI self-documenting, and dissolves finding 2 organically. The `cli-design` skill backs this (modes → subcommands). Medium; the underlying `itest::*` fns are already separate, so it's a dispatch-layer reshuffle. |
| 2 | E/F | low→med | **`itest::run` has 10 positional args** behind `#[allow(clippy::too_many_arguments, reason = "…refactor when more land")]` — **and more has landed.** The allow's own justification has expired. | `itest.rs:123-134` | A `RunConfig`/`ItestRunOpts` struct. Largely subsumed by finding 1 (the `itest run` subcommand still carries ~10 opts → give it a config struct). Drop the `#[allow]` once split. |
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
