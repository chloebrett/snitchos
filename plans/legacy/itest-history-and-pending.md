# Design: per-iteration run history + pending-file baseline workflow

**Status: SHIPPED — plan complete (verified 2026-07-17).** Steps A–H all landed.
`.itest-runs/` is the live per-run history (and the first thing to read when an itest
fails — see CLAUDE.md). The baseline is a projection: `promote_pending` /
`discard_pending` (`baseline.rs:276`, `:303`) + `xtask itest recover`.
H1 (Prometheus textfile, `prom.rs::render_prometheus` + atomic write) and H2 (live
OTLP push, `otlp.rs::push_with_timeout`) shipped too, with a provisioned Grafana
dashboard at `stack/grafana/provisioning/dashboards/snitchos-itest-baselines.json`.

**Every open question resolved — each the way this doc leaned:**

| question | resolution | evidence |
|---|---|---|
| `ctrlc` crate vs raw `libc::signal` | **`ctrlc`** | `xtask/Cargo.toml`; harness takes `interrupt: Option<&AtomicBool>`, xtask sets it from the handler |
| per-iteration `flush()` vs buffered | **flush each row**, no `sync_all` (best-effort) | `history.rs::append` |
| `metadata.toml` `hostname` | **included**, best-effort from `HOSTNAME` | `history.rs::current_hostname` |
| pending shape | **extended `Baseline`** with `partial: Option<PartialMarker>` | `baseline.rs:76,89` |
| auto-prune vs explicit | **explicit only** — `xtask itest prune --keep-last N` | `history.rs::prune_runs`; guards against deleting user-placed files |
| run-dir name format | **timestamp**, second precision (the itest lock rules out collisions, so no nanoseconds) | `history.rs::run_dir_name` |
| disk pressure | **accepted + guarded** by `prune_runs` | — |

Two changes that compose, designed together so we don't paint ourselves into a corner. The pending file is what makes interrupted runs safe; the per-iteration NDJSON is what makes the baseline a *projection* (re-derivable) rather than an independent source of truth.

## goals

1. **Never silently clobber a real baseline with a partial run.** Ctrl-C, OOM, kernel panic, anything — the existing `.itest-baseline.toml` is only replaced by a deliberate human action.
2. **Save every iteration's outcome** (scenario, duration, pass/fail) to a structured file as it happens, so summary stats can be re-derived later with different parameters (different percentiles, time windows, etc.).
3. **Preserve failure logs**, indexed by run + scenario + iteration, so a flake can be investigated weeks later.
4. **Crash-resistant by construction.** Append-only writes. No "rolling" updates that could corrupt mid-write. No assumption that anything other than the last completed iteration was written.

## non-goals

- Resume capability (`--resume`). Out of scope; can be added later on top of this structure if needed.
- Cross-machine baseline sync. The files live in the repo or per-checkout, not in a central database.
- A query / reporting UI. `jq`-able NDJSON is enough until we know what views matter.
- Log compression. Logs aren't that big yet; defer until they are.

## storage model

```
repo-root/
  .itest-baseline.toml              ← tier 1, committed. summary.
  .itest-baseline.toml.pending      ← tier 1.5, gitignored. unpromoted partial.
  .itest-runs/                      ← tiers 2+3, gitignored.
    2026-06-08T12-30-15Z/           ← one directory per run
      metadata.toml
      iterations.ndjson
      fail-heartbeat-cadence-2.log
      fail-heartbeat-cadence-7.log
      ...
    2026-06-08T14-22-01Z/
      ...
```

`.itest-runs/` is gitignored. Per-run directory name is the run start timestamp (UTC, RFC 3339 with colons → dashes for filesystem-safety).

### tier 1: `.itest-baseline.toml`

Unchanged shape. Becomes a *projection* of NDJSON for the relevant runs — computed at promotion time, not maintained incrementally.

### tier 1.5: `.itest-baseline.toml.pending`

Same TOML schema as the baseline file. Written on Ctrl-C exit when `--update-baseline` was passed. Never overwrites the canonical baseline — explicit promotion required.

```toml
# .itest-baseline.toml.pending
# Partial baseline from an interrupted run.
# Inspect, then promote with `cargo xtask itest --promote-pending` or discard with --discard-pending.
[scenarios.heartbeat-cadence.current]
commit = "abc1234"
runs = 487       # ← short of the 1000 requested
failures = 23
recorded_at = "2026-06-08T12:30:15Z"
mean_duration_ms = 1247.0
p95_duration_ms = 1500.0

[scenarios.heartbeat-cadence.current.partial]
requested_runs = 1000
interrupted_at = "2026-06-08T13:15:42Z"
run_dir = ".itest-runs/2026-06-08T12-30-15Z"
```

The `[scenarios.X.current.partial]` table marks the entry as partial. `--baseline-show` surfaces this prominently when reading a pending file. The corresponding `.itest-runs/<dir>/` is *not* deleted on promote/discard — that's tier 2's lifecycle, separate.

### tier 2: `iterations.ndjson`

One row per (scenario invocation). Append-only. Written after the scenario returns, before the next iteration starts.

```json
{"iteration":1,"scenario":"heartbeat-cadence","started_at":"2026-06-08T12:30:15.123Z","duration_ms":1247,"result":"pass"}
{"iteration":1,"scenario":"smp-spawn-on-hart-1-runs","started_at":"2026-06-08T12:30:17.811Z","duration_ms":2342,"result":"pass"}
{"iteration":2,"scenario":"heartbeat-cadence","started_at":"2026-06-08T12:30:20.901Z","duration_ms":1289,"result":"fail","error":"no second heartbeat within 20s","log":"fail-heartbeat-cadence-2.log"}
{"iteration":2,"scenario":"smp-spawn-on-hart-1-runs","started_at":"2026-06-08T12:30:22.245Z","duration_ms":2401,"result":"pass"}
```

Fields:

| field | type | always present | notes |
|---|---|---|---|
| `iteration` | u32 | yes | 1-indexed; matches the runner's per-repeat counter |
| `scenario` | string | yes | scenario name |
| `started_at` | RFC 3339 string | yes | wall-clock at the start of this scenario invocation |
| `duration_ms` | u32 | yes | wall-clock elapsed for this scenario, milliseconds |
| `result` | `"pass"` \| `"fail"` | yes | |
| `error` | string | only on fail | the scenario's returned error message |
| `log` | string | only on fail | relative path (within the run-dir) to the saved log file |

Choosing NDJSON over TOML/binary:

- Append-only writes are trivial: open in append mode, write line, optional fsync. No rewrite.
- Self-contained lines mean a partial / truncated file is still valid up to the last complete line.
- `jq` works directly. So does `wc -l` for "how many iterations did we get to?"
- Streaming reads when re-deriving stats — no need to load 1 MB into memory.

### tier 3: `fail-<scenario>-<iteration>.log`

The full QEMU log captured during this scenario invocation. Copied from `/tmp/snitch-itest-<label>-<pid>.log` (where it currently lives) at the point of failure. Successful scenarios discard their logs as today.

Filename is `fail-<scenario>-<iteration>.log` so it's listable / pattern-matchable. Iteration number is from the runner; uniquely identifies the log within the run.

### tier 2.5: `metadata.toml`

Once per run-directory, written at run start, never modified:

```toml
[run]
started_at = "2026-06-08T12:30:15Z"
commit = "abc1234"
build_hash = "sha256:8d3f..."   # optional
requested_repeat = 1000
fail_fast = 3                   # optional
scenarios = ["heartbeat-cadence", "smp-spawn-on-hart-1-runs", ...]
hostname = "chloe-laptop"       # optional, lets you tell mac vs CI later
```

Lets a `--history-stats` command know what to make of the NDJSON (e.g., "this run requested 1000 iterations but stopped at 487 — partial or fail-fast?"). The "stopped at" is derivable from the iterations file.

## lifecycle

### at run start (the runner's `run()` function)

1. Acquire the integration-test lock (existing behavior).
2. Create `.itest-runs/<timestamp>/` directory.
3. Write `metadata.toml`.
4. Open `iterations.ndjson` in append mode; keep the handle.
5. Continue with the existing build hook, etc.

### during the run, per scenario invocation

After each scenario returns:

1. Record `started_at` (Instant captured before scenario), `duration_ms`, `result`.
2. If failed: copy the log file from its `/tmp` location into the run-dir, capture relative path.
3. Append one NDJSON line. Best-effort `flush()` (not `sync_all`) so we tolerate occasional crashes losing the last 1-2 lines without paying fsync cost per iteration.

### at run end (three exit paths)

**A. Normal completion** (all requested iterations done, no Ctrl-C):

1. Read the NDJSON back; compute summary stats.
2. If `--update-baseline` was passed: build `Baseline` per scenario from the NDJSON projection; call `BaselineFile::update_current` for each; save directly to `.itest-baseline.toml` (no pending file).
3. Run-directory stays on disk. Pruning runs at next start.

**B. Graceful interrupt** (Ctrl-C at iteration boundary):

1. Print "Interrupted after N of M iterations. Saving partial baseline to `.itest-baseline.toml.pending`."
2. Read the NDJSON; compute summary.
3. If `--update-baseline`: build `Baseline` per scenario (with `partial` table set); write to `.itest-baseline.toml.pending` (never the canonical file).
4. Exit code 130 (conventional for SIGINT).

**C. Crash / force-quit** (second Ctrl-C, OOM, panic):

1. Nothing the runner can do.
2. Recovery is from the NDJSON: at any later time, `cargo xtask itest --recover-pending <run-dir>` reads that dir's NDJSON and writes `.itest-baseline.toml.pending`. User then promotes or discards.
3. This means "tier 1.5" can be reconstructed from "tier 2" — they're not redundant, but tier 2 is the load-bearing record.

### promote / discard

User-driven, post-hoc:

```
cargo xtask itest --promote-pending
cargo xtask itest --discard-pending
```

Promote moves the pending file to the canonical baseline location (pushing the previous `current` to `history` as usual). Discard just deletes the pending file. Both are idempotent and operate only on the pending file — the run directory is unaffected (governed by pruning).

`--baseline-show` always reads the canonical file (default) but warns when a pending file exists with hint to inspect.

## APIs in itest-harness

### new module: `history`

```rust
pub mod history {
    /// A live writer to `iterations.ndjson`. Holds the file handle
    /// open across iterations. Drop closes; explicit `flush` is
    /// best-effort.
    pub struct HistoryWriter { /* ... */ }
    impl HistoryWriter {
        pub fn create(run_dir: &Path) -> io::Result<Self>;
        pub fn append(&mut self, row: &IterationRow) -> io::Result<()>;
    }

    /// One row in `iterations.ndjson`.
    #[derive(Serialize, Deserialize)]
    pub struct IterationRow {
        pub iteration: u32,
        pub scenario: String,
        pub started_at: OffsetDateTime,
        pub duration_ms: u32,
        pub result: ResultKind,
        pub error: Option<String>,
        pub log: Option<String>,
    }

    #[derive(Serialize, Deserialize, PartialEq)]
    #[serde(rename_all = "snake_case")]
    pub enum ResultKind { Pass, Fail }

    /// Streaming read of an NDJSON file.
    pub fn read_iterations(path: &Path) -> io::Result<impl Iterator<Item = io::Result<IterationRow>>>;

    /// Compute per-scenario aggregates from an iteration stream.
    /// Returns the same shape `Aggregator` produces — handy for
    /// re-deriving baselines.
    pub fn aggregate_stream(rows: impl Iterator<Item = IterationRow>) -> Aggregator;
}
```

### `RunnerConfig` additions

```rust
pub struct RunnerConfig<'a> {
    // ... existing fields ...

    /// Root directory for per-run history (tier 2/3). If `None`,
    /// no history is written. xtask sets this to `.itest-runs/`.
    pub history_root: Option<PathBuf>,

    /// Signal flag for graceful interrupt. Set by an external signal
    /// handler; the runner checks at iteration boundaries and exits
    /// after writing the pending baseline.
    pub interrupt: Option<&'a AtomicBool>,

    /// On graceful interrupt with `update_baseline=true`, where to
    /// write the partial baseline. xtask sets this to
    /// `.itest-baseline.toml.pending`. Required for the interrupted
    /// path; ignored otherwise.
    pub pending_baseline: Option<PathBuf>,
}
```

### `BaselineFile` additions

The `Baseline` struct gets an optional partial-marker:

```rust
pub struct Baseline {
    // ... existing fields ...

    /// Present only when this baseline reflects an interrupted run.
    /// Promotion still works — the field gets stripped — but
    /// `--baseline-show` surfaces partial entries differently.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partial: Option<PartialMarker>,
}

pub struct PartialMarker {
    pub requested_runs: u32,
    pub interrupted_at: OffsetDateTime,
    pub run_dir: String,
}
```

`BaselineFile` gets:

```rust
impl BaselineFile {
    /// Load from `<path>.pending`, returning `Ok(None)` if no
    /// pending file exists.
    pub fn load_pending(path: &Path) -> Result<Option<Self>, BaselineError>;

    /// Promote the pending file to canonical. Returns the previous
    /// canonical content (now archived to history per usual
    /// `update_current` semantics).
    pub fn promote_pending(canonical_path: &Path) -> Result<Self, BaselineError>;

    /// Delete the pending file.
    pub fn discard_pending(canonical_path: &Path) -> io::Result<()>;
}
```

### xtask CLI additions

```
cargo xtask itest --repeat N --update-baseline
  # normal run, auto-promote on completion, pending on interrupt

cargo xtask itest --baseline-show
  # canonical file; warns if pending exists

cargo xtask itest --promote-pending
cargo xtask itest --discard-pending
cargo xtask itest --recover-pending <run-dir>
  # rebuild pending from NDJSON, e.g. after a force-quit
cargo xtask itest --prune-runs [--keep-last N | --max-size N MiB]
  # tier 2/3 maintenance
```

## crash model

The two "weak spots" worth being explicit about:

1. **Between `started_at` capture and NDJSON write.** If the process dies mid-scenario, that iteration won't be in NDJSON. Acceptable — partial mid-iteration data is meaningless anyway.
2. **Between NDJSON flush and OS write to disk.** The default `flush()` after each row is best-effort; we don't `sync_all`. A power loss could lose the last few rows. Acceptable for this workload (rare, low cost). If it ever matters, `--sync-history` flag flips to `sync_all`.

Pending baseline is *only* written on graceful interrupt. If the process crashes during pending-write, the pending file may be incomplete TOML — user runs `--recover-pending <run-dir>` to rebuild from NDJSON. Idempotent.

## migration / compat

- Existing `.itest-baseline.toml` files keep working without `partial` field (it's `#[serde(default)]`).
- Existing runs that don't write history are unaffected by `history_root: None`.
- Pending file is new — no old version. `--promote-pending` errors clearly if no pending file exists.
- `.gitignore` gets `.itest-runs/` and `.itest-baseline.toml.pending` added (`.itest.lock` already there).

## step-by-step build order

Each step is independently shippable; each leaves the suite green.

### step A: pending-file workflow (no history yet)

1. Add `partial: Option<PartialMarker>` field to `Baseline`. Round-trip test.
2. Add `pending_baseline: Option<PathBuf>` to `RunnerConfig`. When set and `update_baseline=true`, write pending on… well, today there's no interrupt. So step A just establishes the field; it's a no-op for now.

Punt step A's interesting behavior until interrupt arrives in step B.

### step B: graceful interrupt + pending write

1. Add `interrupt: Option<&AtomicBool>` to `RunnerConfig`.
2. Runner checks `interrupt` at iteration boundary; if set, breaks the loop.
3. After the loop, if interrupted and `update_baseline`: write to `pending_baseline` path instead of canonical.
4. xtask: install `ctrlc::set_handler` (yes, new dep) that sets an AtomicBool. Print the "interrupted, saving pending" message on first interrupt.
5. xtask CLI: `--promote-pending`, `--discard-pending`. These can be one-flag-per-action since this is a single mode (no scenario list etc.).
6. Tests: a fake-interrupt test in itest-harness (set the AtomicBool before calling `run`, verify pending was written).

### step C: NDJSON writer + per-iteration records

1. New `itest_harness::history` module with `HistoryWriter`.
2. Add `history_root: Option<PathBuf>` to `RunnerConfig`.
3. Runner: at start, create the run-dir, write `metadata.toml`, open `HistoryWriter`. Per scenario: append a row. At end: do nothing special — history is its own record.
4. xtask: set `history_root: Some(".itest-runs".into())`.
5. Tests: build a tempdir, run with synthetic scenarios, parse the NDJSON back and assert content.
6. `.gitignore` `.itest-runs/`.

### step D: failure log capture

1. On scenario failure, the runner already has the log path (from `log_path_for` hook). Copy that file into the run-dir as `fail-<scenario>-<iteration>.log`.
2. Record the relative path in the NDJSON row's `log` field.
3. Test: synthetic failure scenario that writes a known log file; verify the copy lands and the NDJSON points at it.

### step E: aggregate-from-NDJSON

1. `history::aggregate_stream` builds an `Aggregator` from a row iterator.
2. Runner switches: instead of maintaining `Aggregator` incrementally, drive everything off NDJSON re-reads at exit time. (Or keep both — the incremental Aggregator stays the working copy; NDJSON is for crash-recovery and re-derivation.)
3. `--recover-pending <run-dir>` command — reads metadata + NDJSON from a run-dir, builds `Aggregator`, writes pending baseline.

### step F: pruning

1. `cargo xtask itest --prune-runs --keep-last N` — sort `.itest-runs/` entries by name (timestamp-sortable), delete oldest beyond N.
2. Optional `--max-size` variant.
3. Tests on a tempdir with synthetic run-dirs.

### step G: `--baseline-show` partial awareness

1. When canonical and pending both exist: show canonical with a warning at top: "pending baseline also present at <path>. Run `--promote-pending` or `--discard-pending` to clear."
2. `--baseline-show --pending` flag to inspect the pending file specifically (partial entries clearly marked).

### step H: CI flake metrics into Grafana

Closes the observability loop: the same Tempo/Prometheus/Grafana stack that ingests kernel telemetry also surfaces CI-side flake metrics. Designed in two sub-steps so the cheap, useful thing lands first.

**H1: batch export (NDJSON → Prometheus textfile)**

Add `cargo xtask itest --emit-metrics <path>` (or default to `target/ci-metrics.prom`). Reads `.itest-baseline.toml` plus the most recent N run directories' NDJSON; emits a Prometheus exposition file:

```
# HELP ci_baseline_failure_rate Per-scenario rate from .itest-baseline.toml current.
# TYPE ci_baseline_failure_rate gauge
ci_baseline_failure_rate{scenario="heartbeat-cadence"} 0.06

# HELP ci_iteration_duration_seconds Per-scenario per-iteration wall-clock duration.
# TYPE ci_iteration_duration_seconds histogram
ci_iteration_duration_seconds_bucket{scenario="heartbeat-cadence",le="0.5"} 0
ci_iteration_duration_seconds_bucket{scenario="heartbeat-cadence",le="1.0"} 0
ci_iteration_duration_seconds_bucket{scenario="heartbeat-cadence",le="1.5"} 187
ci_iteration_duration_seconds_bucket{scenario="heartbeat-cadence",le="2.0"} 195
ci_iteration_duration_seconds_bucket{scenario="heartbeat-cadence",le="+Inf"} 200
ci_iteration_duration_seconds_sum{scenario="heartbeat-cadence"} 248.6
ci_iteration_duration_seconds_count{scenario="heartbeat-cadence"} 200

# HELP ci_iterations_total Cumulative iteration count per scenario+result.
# TYPE ci_iterations_total counter
ci_iterations_total{scenario="heartbeat-cadence",result="pass"} 188
ci_iterations_total{scenario="heartbeat-cadence",result="fail"} 12
```

Prometheus `node_exporter`'s textfile collector (or `--collector.textfile.directory`) scrapes the file. No new networking, no new ports — Prometheus just reads the file on its scrape interval.

Pros: trivially testable; zero runtime overhead during itest runs; metrics survive process death (it's just a file).
Cons: not real-time during a long run; metrics only update when explicitly emitted.

**H2: live OTLP push during runs**

Layered on top of H1. `xtask itest` opens an OTLP HTTP connection at run start and pushes per-iteration observations as they happen. NDJSON still writes (it's the durable record); OTLP push is parallel.

- Dep: `opentelemetry-rust` + `opentelemetry-otlp` HTTP exporter.
- Resilient to collector being down — buffer locally, drop if buffer overflows, log a warning.
- Both run-level info (commit, hostname, requested_repeat) and per-iteration data flow.

Pros: real-time Grafana updates during 3-hour baseline runs.
Cons: more dep surface; needs the collector reachable.

**Grafana dashboards** (independent of H1 vs H2):

- *Per-scenario flake rate over time* — heatmap, scenarios on Y, time on X, color = failure rate.
- *Iteration duration distribution per scenario* — histogram percentile lines (p50/p95/p99) over time. This is the view that makes bimodal timing distributions and creeping regressions immediately visible.
- *Baseline-vs-current comparison panel* — current run's `current` baseline vs. previous, with statistical confidence band overlaid.
- *Run summary card* — last N runs, link to NDJSON paths for forensic dive-in.

These are why H exists. The numerical baseline comparison we have today answers "is this regressing?" Grafana answers "what's the *shape* of the regression?"

**Dependencies on prior steps**: H1 needs C (NDJSON writer) and the existing baseline machinery. H2 needs H1's metric model definitions but otherwise stands alone. Both depend on the Tempo/Prometheus/Grafana stack already in `stack/docker-compose.yml`.

## open decisions

These are things to lock in as we implement:

- [ ] Dep on `ctrlc` crate vs raw `libc::signal`. `ctrlc` is small and well-tested; I'd take the dep.
- [ ] Per-iteration `flush()` or buffered? `flush()` is honest about durability and the cost is negligible at our iteration rate. Default: flush each row.
- [ ] `metadata.toml` `hostname` field — useful or noise? I'd include it; trivial to add and helps disambiguate later.
- [ ] Pending-file shape: extend `Baseline` with `partial: Option<…>`, or use a sibling type? I lean toward extending — same serializer, smaller surface.
- [ ] Auto-prune on next run start, or only when explicitly invoked? I lean toward "warn if `.itest-runs/` exceeds N entries; prune only on explicit command." Easier to debug than surprise deletions.
- [ ] Run-directory name format. `2026-06-08T12-30-15Z` is filesystem-safe and sortable. Alternative: `run-<commit>-<n>` with a counter. Timestamps win for "what happened on Tuesday afternoon."
- [ ] Are we OK with the additional disk pressure on a busy laptop? At ~1 MB / 1000 iterations of NDJSON + a few MB of fail logs, a 20-run history is ≤ 100 MB. Tolerable; document the cap.
