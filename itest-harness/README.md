# itest-harness

Host-buildable integration-test harness for SnitchOS. Owns the
platform-pure runner mechanics: per-scenario flake-rate aggregation,
Wilson-score confidence intervals, two-proportion z-test for
regressions, baseline-file persistence, per-iteration NDJSON history,
captured failure logs, pending-baseline workflow for interrupted runs,
and Prometheus / OTLP exports.

`cargo xtask itest` is the user-facing entry point — see
[`../xtask/README.md`](../xtask/README.md) for the workflow. This crate
is what `xtask` calls into.

Pure-host, no `riscv64`-specific code — every change is verifiable with
`cargo test -p itest-harness` in well under a second, no QEMU boot
required.

## Module map

| Module | What lives here |
|---|---|
| `runner` | Top-level scenario loop, `RunnerConfig` hooks, SIGINT plumbing |
| `aggregate` | Per-scenario run/failure/duration aggregator + p95 |
| `baseline` | `.itest-baseline.toml` schema, load/save, pending-sidecar, `from_recovered`, `render_summary` |
| `history` | `.itest-runs/<ts>/` layout: `metadata.toml`, NDJSON writer, `aggregate_run_dir`, `prune_runs` |
| `lock` | `ItestLock` (file-lock + PID stamp) |
| `stats` | Wilson-score 95% CI, two-proportion pooled z-test, normal CDF |
| `verdict` | Baseline-vs-current comparison: `NoBaseline`, `Consistent`, `Different{Worse,Better}` + `render_comparison` |
| `prom` | Prometheus textfile-format export + atomic write |
| `otlp` | Hand-rolled OTLP/HTTP metrics proto subset + push |

## Why a separate crate

`xtask` historically owned both the runner mechanics AND the
QEMU-spawning glue. Mixing them made the runner hard to test —
`xtask` pulls in `clap` and the QEMU launcher, neither of which has
any place inside a unit test for "does the aggregator compute the
right p95?".

Lifting the platform-pure parts into a separate workspace member
mirrors the `kernel` / `kernel-core` split: `xtask` keeps the
unbuildable-without-QEMU stuff, `itest-harness` keeps the
unit-testable stuff. Plugging in a non-QEMU subject (or a different
event stream) is a matter of passing a different `RunnerConfig`.

## Design docs

- `plans/itest-harness-extraction.md` — the migration plan that
  carved this crate out of `xtask`.
- `plans/itest-history-and-pending.md` — the history / pending /
  export design (tiers 1 through 3, Grafana ingestion path).
