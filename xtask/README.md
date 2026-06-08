# xtask

Build / run / test orchestration for SnitchOS. Invoked as
`cargo xtask <subcommand>`. Wraps `cargo`, QEMU, docker-compose, and
the [`itest-harness`](../itest-harness/README.md) library.

## Subcommands

| Command | What it does |
|---|---|
| `cargo xtask build` | Build the kernel ELF for `riscv64gc-unknown-none-elf` |
| `cargo xtask boot` | Build + run the kernel in QEMU. Telemetry chardev waits for a client. |
| `cargo xtask debug --features <feat>` | Same as `boot` but with `-s -S`. Prints attach commands. |
| `cargo xtask collect [-- args]` | Build + run the host-side collector (OTLP + Loki + Prometheus) |
| `cargo xtask reader` | Collector in text-only mode (no docker stack needed) |
| `cargo xtask stack {up,down,logs}` | docker-compose the Tempo + Prometheus + Loki + Grafana stack |
| `cargo xtask test` | Run host unit tests across the workspace |
| `cargo xtask itest [...]` | Kernel integration tests in QEMU. See below. |
| `cargo xtask clippy [-- args]` | Lint the WHOLE workspace correctly (kernel for riscv, host for host) |
| `cargo xtask mutants [-- args]` | Mutation testing with the right config + feature flags |
| `cargo xtask loc` | Lines-of-code by crate, split production vs test |

## Integration tests: `cargo xtask itest`

The integration suite boots the kernel in QEMU, reads `Frame`s off
the virtio-console socket, and asserts on the decoded sequence.
Scenarios are defined in `xtask/src/itest/scenarios.rs`; the runner
mechanics, statistics, baselines, history and exports live in
[`itest-harness`](../itest-harness/README.md).

### Running scenarios

```bash
cargo xtask itest                                # every scenario, once
cargo xtask itest <name>                         # single scenario
cargo xtask itest --repeat 100                   # flake-hunt
cargo xtask itest --repeat 1000 --fail-fast 3    # bail after 3 failures
cargo xtask itest --skip-unit-tests              # skip cargo test -p kernel-core
cargo xtask itest --force                        # ignore the .itest.lock mutex
```

One kernel build per invocation — the harness builds once at startup
and reuses the ELF across every iteration. A `cargo build` you
trigger in another shell mid-run will NOT take effect.

Concurrent `itest` invocations from the same checkout are blocked by
`.itest.lock` (a file-lock plus PID stamp). The first holder wins;
later ones print which PID is holding it and exit. `--force`
overrides if you know the lock is stale.

### Comparing against a baseline

`.itest-baseline.toml` lives at the repo root and carries
per-scenario `(commit, runs, failures, timing)` rows. After every
`itest` run, the harness compares the current run's per-scenario
rates against the baseline using a pooled two-proportion z-test and
prints a verdict (`Consistent`, `Worse`, `Better`).

```bash
cargo xtask itest --update-baseline --repeat 100      # rewrite baseline from this run
cargo xtask itest --baseline-show                     # render the canonical file
cargo xtask itest --baseline-show --include-history   # show prior currents too
cargo xtask itest --baseline-show --flakes-only       # only nonzero-failure scenarios
```

`--flakes-only` sorts by Wilson-score 95% CI lower bound
(descending; upper bound as tie-break). The most-confidently-flaky
scenario floats to the top.

### Interrupted runs

Hit Ctrl-C during `--update-baseline` and the harness writes
`.itest-baseline.toml.pending` instead of clobbering the canonical
file. A second Ctrl-C in the same handler force-quits without
writing anything.

```bash
cargo xtask itest --baseline-show              # banners if a pending file exists
cargo xtask itest --baseline-show --pending    # render the .pending sidecar
cargo xtask itest --promote-pending            # accept it as the new baseline
cargo xtask itest --discard-pending            # throw it away
```

Every `--repeat` run also writes a directory under
`.itest-runs/<UTC timestamp>/` containing:

- `metadata.toml` — run-level info (commit, requested repeat, fail-fast, hostname).
- `iterations.ndjson` — one append-only JSON row per iteration.
- `fail-<scenario>-<iter>.log` — copy of the QEMU log for any failed iteration.

A pending sidecar's `partial` marker references this directory by
name. If the process dies before the in-memory pending file gets
written, rebuild it from the NDJSON:

```bash
cargo xtask itest --recover-pending .itest-runs/2026-06-08T12-30-15Z
```

Refuses if a pending file already exists — promote or discard it
first.

### Pruning history

```bash
cargo xtask itest --prune-runs                       # keep 20 newest (default)
cargo xtask itest --prune-runs --keep-last 50        # keep 50
cargo xtask itest --prune-runs --keep-last 0         # nuke all of .itest-runs/
```

Only directories matching the `YYYY-MM-DDTHH-MM-SSZ` shape are
considered — hand-placed files or notes/ subdirs are left alone.

### Exporting baseline metrics to Grafana

Nine gauges per scenario (runs, failures, failure rate, Wilson CI
lower / upper, mean & p95 duration, partial flag, recorded-at). Two
transports, same data.

**A. Prometheus textfile** (works without docker; pair with
`node_exporter --collector.textfile.directory=`):

```bash
cargo xtask itest --export-prom /var/lib/node_exporter/textfile/snitchos-itest.prom
```

Atomic write: tmp file then `rename`, so the scraper can't catch a
half-written file.

**Auto-push at end of run**: By default, every `cargo xtask itest`
run that completes (or is interrupted) tries to push the canonical
baseline to `http://127.0.0.1:9090/api/v1/otlp` and warns if the
endpoint isn't reachable (1s connect timeout). Pass `--no-auto-push`
to silence in CI / scripts.

**B. Live OTLP push** (one-shot, no test run):

```bash
cargo xtask stack up
cargo xtask itest --push-otlp                                        # default endpoint (localhost stack)
cargo xtask itest --push-otlp https://prom.example/api/v1/otlp       # custom endpoint
```

`--push-otlp` without a value targets `http://127.0.0.1:9090/api/v1/otlp`
— the bundled `stack/docker-compose.yml` boots Prometheus with
`--web.enable-otlp-receiver`, so OTLP/HTTP metrics ingest at
`/api/v1/otlp/v1/metrics`. `/v1/metrics` is appended automatically
if missing. Wire `--push-otlp` into a cron entry, a post-run hook,
or a CI step.

### Metric name reference

Prometheus rewrites incoming metric names: `.` → `_`, and (for the
OTLP receiver path) the unit suffix gets appended. Same data, two
shapes:

| Quantity | Textfile (`--export-prom`) | OTLP push (`--push-otlp`) |
|---|---|---|
| Runs in current baseline | `snitchos_itest_baseline_runs` | `snitchos_itest_baseline_runs` |
| Failures | `snitchos_itest_baseline_failures` | `snitchos_itest_baseline_failures_ratio` |
| Failure rate `[0,1]` | `snitchos_itest_baseline_failure_rate` | `snitchos_itest_baseline_failure_rate_ratio` |
| Wilson CI lower / upper | `..._wilson_lower` / `..._wilson_upper` | `..._wilson_lower_ratio` / `..._wilson_upper_ratio` |
| Mean / p95 duration (ms) | `..._mean_duration_ms` / `..._p95_duration_ms` | `..._mean_duration_ms_milliseconds` / `..._p95_duration_ms_milliseconds` |
| Partial flag (0 or 1) | `..._partial` | `..._partial_ratio` |
| Recorded-at (unix s) | `..._recorded_at_seconds` | `..._recorded_at_seconds` |

Every series carries a `scenario="..."` label.

A provisioned Grafana dashboard ("SnitchOS — itest baselines") shows
the canonical queries; auto-loaded on `stack up`. PromQL cheat
sheet:

```promql
topk(5, snitchos_itest_baseline_wilson_lower_ratio)               # most-confidently flaky
snitchos_itest_baseline_failure_rate_ratio > 0.01                 # flaking > 1%
topk(5, snitchos_itest_baseline_p95_duration_ms_milliseconds)     # slow scenarios
snitchos_itest_baseline_partial_ratio == 1                        # interrupted runs you forgot
```

## clippy caveats

```bash
cargo xtask clippy [-- args]
```

Use this, not `cargo clippy --workspace`. The kernel only builds
for `riscv64gc-unknown-none-elf`; a plain workspace clippy would
compile it for the host, where it can't link. `xtask clippy` lints
host crates for the host and the kernel for riscv in one go.

Don't blanket `--fix` the kernel — clippy's `deref_addrof` autofix
rewrites the required `&mut *(&raw mut STATIC)` idiom into a
forbidden direct `&mut STATIC`. Those sites carry a justified
`#[allow(clippy::deref_addrof, reason = ...)]`.
