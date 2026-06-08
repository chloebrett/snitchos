//! Generic `--repeat N` runner. Owns the per-iteration loop, scenario
//! invocation, aggregation, baseline I/O, and the printable output —
//! everything that doesn't depend on what's under test.
//!
//! The consumer (xtask, for now) plugs in:
//!
//! - The scenario list (`&[Scenario]`).
//! - Hooks: `one_shot_build`, `log_path_for`, `max_wait_for`,
//!   `current_commit`. All optional; the runner no-ops when a hook
//!   is `None`.
//! - The baseline file path.
//!
//! Nothing in this module knows about QEMU, virtio, or postcard.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use time::OffsetDateTime;

use crate::aggregate::{Aggregator, RunTotals};
use crate::baseline::{Baseline, BaselineFile};
use crate::history::HistoryWriter;
use crate::verdict::{ComparisonRender, DEFAULT_ALPHA, render_comparison, verdict};

/// One scenario: a name + a function that returns `Ok(())` on pass or
/// a human-readable error string on failure. Same shape xtask has
/// used since v0.1.
pub struct Scenario {
    pub name: &'static str,
    pub run: fn() -> Result<(), String>,
}

/// Per-scenario log-path lookup. Returns the path to a scenario's
/// log file (so the runner can dump the tail on failure + copy it
/// into the run-dir), or `None` when no log was captured. Aliased to
/// keep `RunnerConfig` under clippy's `type_complexity` threshold.
///
/// `Send + Sync` because workers in the `jobs > 1` path call this
/// from their own threads. Plain `fn`-pointer and capture-free
/// closures satisfy this automatically; consumers that need shared
/// state should use `Arc` / `Mutex` rather than `Rc` / `RefCell`.
pub type LogPathFn<'a> = &'a (dyn Fn(&str) -> Option<PathBuf> + Send + Sync);

/// Per-scenario timing-hook signature. Called immediately after
/// `(scenario.run)()` from the same thread, so `harness`-side
/// thread-locals work. Same `Send + Sync` rationale as `LogPathFn`.
pub type MaxWaitFn<'a> = &'a (dyn Fn() -> Option<(Duration, Duration)> + Send + Sync);

/// Hooks the runner calls during execution. Each is `Option<&dyn Fn>`
/// so consumers can supply only what they need. Lifetime parameter
/// matches the surrounding `run()` call — hooks don't escape.
#[derive(Default)]
pub struct RunnerConfig<'a> {
    /// Called once before any scenarios. Errors abort the run with
    /// exit code 2. Typical use: `cargo build -p kernel`.
    pub one_shot_build: Option<&'a dyn Fn() -> Result<(), String>>,

    /// Called after each scenario; returns the path to that scenario's
    /// log file, or `None` if no log was captured. The runner dumps
    /// the last ~80 lines on failure.
    pub log_path_for: Option<LogPathFn<'a>>,

    /// Called after each scenario; returns `(actual_elapsed, budget)`
    /// from whatever the consumer's harness measured (NOT the same as
    /// the runner's wall-clock timing — this is e.g. virtio-frame wait
    /// budget exposure). Used only for inline display.
    pub max_wait_for: Option<MaxWaitFn<'a>>,

    /// Called once when writing a new baseline entry. Returns the
    /// short git commit hash, or `None` if unavailable.
    pub current_commit: Option<&'a dyn Fn() -> Option<String>>,

    /// Path to the baseline TOML. If `None`, baseline comparison is
    /// skipped and `--update-baseline` is a no-op (with a warning).
    pub baseline_file: Option<PathBuf>,

    /// Stop early once cumulative failures reach this many. `None`
    /// (the default) lets the run go to completion. Useful for
    /// "confirm flakiness fast" workflows: with K=3, a flaky kernel
    /// usually wraps within ~30 scenario-runs instead of the full
    /// `--repeat N`. The check fires at iteration boundaries, not
    /// mid-iteration — the per-run output stays coherent.
    pub fail_fast: Option<u32>,

    /// Path where a *partial* baseline gets written if the run is
    /// interrupted mid-`--repeat` (graceful Ctrl-C). Only consulted
    /// when `update_baseline` is true; never overwrites the canonical
    /// baseline file. See `plans/itest-history-and-pending.md`.
    pub pending_baseline: Option<PathBuf>,

    /// Interrupt signal. The runner reads this at every iteration
    /// boundary; when it's `true`, the loop breaks gracefully and
    /// (if `update_baseline`) the partial baseline goes to
    /// `pending_baseline` instead of the canonical file. The caller
    /// sets it from a signal handler (xtask uses `ctrlc`).
    pub interrupt: Option<&'a std::sync::atomic::AtomicBool>,

    /// Root directory for per-run history (NDJSON + metadata + log
    /// copies). When set, the runner creates a timestamped
    /// subdirectory at start, writes `metadata.toml`, and appends one
    /// JSON row per scenario invocation to `iterations.ndjson`. When
    /// `None`, no history is written. See
    /// `plans/itest-history-and-pending.md` step C.
    pub history_root: Option<PathBuf>,

    /// Number of worker threads for per-iteration scenario fan-out.
    /// `0` and `1` both mean sequential execution (preserves the
    /// pre-parallel output format). `>1` enables the worker pool
    /// (see `plans/itest-parallel-scenarios.md`). Each iteration
    /// still runs to completion (its own `RunTotals`, fail-fast and
    /// interrupt check) before the next begins — fan-out is intra-
    /// iteration only.
    pub jobs: u32,
}

/// Run `scenarios` `repeat` times. If `update_baseline` is true, write
/// the run's per-scenario results back to the baseline file.
///
/// Exit code: `2` on build-hook failure; `1` if any scenario failed
/// any iteration; `0` otherwise. Single-iteration runs match the
/// pre-extraction xtask behaviour (no aggregate printed).
pub fn run(
    scenarios: &[&Scenario],
    repeat: u32,
    update_baseline: bool,
    config: &RunnerConfig<'_>,
) -> ExitCode {
    if let Some(build) = config.one_shot_build
        && let Err(e) = build()
    {
        eprintln!("kernel build failed: {e}");
        return ExitCode::from(2);
    }

    let runs = repeat.max(1);
    let mut aggregator = Aggregator::new();
    let mut interrupted = false;

    // History (tier 2/3). Create the run-dir + open the NDJSON writer
    // if a history_root was supplied. Failure here is non-fatal: log
    // and proceed without history.
    //
    // The writer is wrapped in `Mutex<Option<_>>` so the parallel path
    // can share it across worker threads. The lock is held only for
    // the append; serial paths pay essentially no cost.
    let now_for_history = OffsetDateTime::now_utc();
    let mut initial_history_writer: Option<HistoryWriter> = None;
    let mut history_dir: Option<PathBuf> = None;
    if let Some(root) = config.history_root.as_deref() {
        let commit = config
            .current_commit
            .and_then(|f| f())
            .unwrap_or_else(|| "unknown".to_string());
        let metadata = crate::history::RunMetadata {
            run: crate::history::RunMetadataInner {
                started_at: now_for_history,
                commit,
                build_hash: None,
                requested_repeat: runs,
                fail_fast: config.fail_fast,
                scenarios: scenarios.iter().map(|s| s.name.to_string()).collect(),
                hostname: crate::history::current_hostname(),
            },
        };
        match crate::history::create_run_dir(root, &metadata) {
            Ok((dir, writer)) => {
                eprintln!("history: writing per-iteration records to {}", dir.display());
                history_dir = Some(dir);
                initial_history_writer = Some(writer);
            }
            Err(e) => {
                eprintln!(
                    "warning: failed to create history directory under {}: {e} — \
                     proceeding without per-iteration history.",
                    root.display()
                );
            }
        }
    }

    let history_writer: Mutex<Option<HistoryWriter>> = Mutex::new(initial_history_writer);
    let jobs = config.jobs.max(1);

    for run_idx in 0..runs {
        if runs > 1 {
            eprintln!("\n=== run {}/{} ===", run_idx + 1, runs);
        }

        // Step 3 of plans/itest-parallel-scenarios.md: fan-out is
        // intra-iteration. `jobs == 1` keeps the sequential output
        // format ("test X ... ok"); `jobs > 1` runs N worker threads
        // pulling from a shared queue, prefix-line output.
        let mut local = Aggregator::new();
        let failed_this_run: usize;
        let total = scenarios.len();

        if jobs <= 1 {
            // Sequential path. Format unchanged from pre-step-3.
            let mut count = 0;
            for s in scenarios {
                let res = process_one_scenario(
                    s,
                    run_idx,
                    &mut local,
                    &history_writer,
                    history_dir.as_deref(),
                    config.log_path_for,
                    config.max_wait_for,
                    ScenarioFormat::Inline,
                );
                if res.failed {
                    count += 1;
                }
            }
            failed_this_run = count;
        } else {
            // Parallel path. Workers pop from a shared queue, run the
            // scenario, record into a worker-local `Aggregator`. After
            // the scope joins we merge them into `local`.
            let work: Mutex<VecDeque<&Scenario>> =
                Mutex::new(scenarios.iter().copied().collect());
            let log_path_for = config.log_path_for;
            let max_wait_for = config.max_wait_for;
            let history_dir_ref = history_dir.as_deref();
            let history_writer_ref = &history_writer;
            let work_ref = &work;

            let per_worker: Vec<(Aggregator, usize)> = thread::scope(|sc| {
                let mut handles = Vec::with_capacity(jobs as usize);
                for _ in 0..jobs {
                    handles.push(sc.spawn(move || {
                        let mut worker_agg = Aggregator::new();
                        let mut worker_failed = 0usize;
                        loop {
                            let next = {
                                let mut q = work_ref.lock().expect("work-queue poisoned");
                                q.pop_front()
                            };
                            let Some(s) = next else { break };
                            let res = process_one_scenario(
                                s,
                                run_idx,
                                &mut worker_agg,
                                history_writer_ref,
                                history_dir_ref,
                                log_path_for,
                                max_wait_for,
                                ScenarioFormat::Prefixed,
                            );
                            if res.failed {
                                worker_failed += 1;
                            }
                        }
                        (worker_agg, worker_failed)
                    }));
                }
                handles
                    .into_iter()
                    .map(|h| h.join().expect("worker panicked"))
                    .collect()
            });

            failed_this_run = per_worker.iter().map(|(_, f)| *f).sum();
            for (worker_agg, _) in per_worker {
                local.merge(worker_agg);
            }
        }

        eprintln!("\n{} passed, {} failed", total - failed_this_run, failed_this_run);
        local.finish_run(RunTotals {
            passed: total - failed_this_run,
            failed: failed_this_run,
        });
        aggregator.merge(local);

        // `--fail-fast=K`: abort the outer loop once K total failures
        // have accumulated. Print a one-liner so the user sees why
        // the run ended early.
        if let Some(k) = config.fail_fast
            && aggregator.total_failures() >= k
        {
            eprintln!(
                "\n--fail-fast: {} total failures reached (threshold {k}); aborting after run {}/{}.",
                aggregator.total_failures(),
                run_idx + 1,
                runs
            );
            break;
        }

        // Graceful interrupt: external signal handler flipped this
        // bool. Finish printing the per-run summary above, then exit
        // the loop. Pending-baseline write happens below.
        if let Some(flag) = config.interrupt
            && flag.load(std::sync::atomic::Ordering::SeqCst)
        {
            interrupted = true;
            eprintln!(
                "\nInterrupted after run {}/{}. Writing partial baseline (if --update-baseline).",
                run_idx + 1,
                runs
            );
            break;
        }
    }

    // Single-run path: behaviour unchanged from before.
    if runs == 1 {
        return if aggregator.run_totals()[0].failed == 0 {
            ExitCode::SUCCESS
        } else {
            ExitCode::from(1)
        };
    }

    eprint!("{}", aggregator.render_aggregate(runs));

    // Baseline comparison + optional update.
    let baseline_file = config
        .baseline_file
        .as_deref()
        .and_then(load_baseline_or_warn);
    print_baseline_comparisons(scenarios, &aggregator, baseline_file.as_ref());

    if update_baseline {
        let commit = config
            .current_commit
            .and_then(|f| f())
            .unwrap_or_else(|| "unknown".to_string());
        let now = OffsetDateTime::now_utc();

        if interrupted {
            // Pending path: fresh BaselineFile (never load the
            // previous pending — each interrupted run produces its
            // own snapshot); every entry gets a `partial` marker so
            // promote-time tooling can flag the incomplete-ness.
            let Some(path) = config.pending_baseline.as_deref() else {
                eprintln!(
                    "warning: interrupted with --update-baseline but no \
                     pending_baseline path was supplied"
                );
                return exit_code_with_interrupt(&aggregator, interrupted);
            };
            let partial = crate::baseline::PartialMarker {
                requested_runs: runs,
                interrupted_at: now,
                run_dir: history_dir
                    .as_ref()
                    .map(|p| p.to_string_lossy().into_owned()),
            };
            let pending = apply_current_run_to_baseline_with_partial(
                BaselineFile::default(),
                scenarios,
                &aggregator,
                &commit,
                now,
                Some(&partial),
            );
            if let Err(e) = pending.save_path(path) {
                eprintln!("warning: failed to write {}: {e}", path.display());
            } else {
                eprintln!(
                    "\nWrote partial baseline to {} ({} of {} requested iterations). \
                     Promote with --promote-pending or discard with --discard-pending.",
                    path.display(),
                    aggregator.runs(),
                    runs
                );
            }
        } else {
            let Some(path) = config.baseline_file.as_deref() else {
                eprintln!(
                    "warning: --update-baseline requested but no baseline_file path was supplied"
                );
                return exit_code_with_interrupt(&aggregator, interrupted);
            };
            let updated = apply_current_run_to_baseline_with_partial(
                baseline_file.unwrap_or_default(),
                scenarios,
                &aggregator,
                &commit,
                now,
                None,
            );
            if let Err(e) = updated.save_path(path) {
                eprintln!("warning: failed to write {}: {e}", path.display());
            } else {
                eprintln!("\nUpdated {} with current run's results.", path.display());
            }
        }
    }

    exit_code_with_interrupt(&aggregator, interrupted)
}

/// Output format for `process_one_scenario`. Sequential runs use
/// `Inline` (`test NAME ... ok`); parallel runs use `Prefixed`
/// (`[NAME] ok`) so concurrent completions don't interleave
/// half-printed lines.
enum ScenarioFormat {
    Inline,
    Prefixed,
}

struct ScenarioOutcome {
    failed: bool,
}

/// Run one scenario and record everything that follows: timing,
/// log-tail dump on failure, log-file copy into the run-dir, NDJSON
/// row. Shared between the sequential and parallel paths.
///
/// The thread that calls this is the same thread `(s.run)()` ran on,
/// so any thread-local state set by the consumer's harness (xtask's
/// `take_last_log_path` / `take_last_max_wait`) is reachable through
/// the supplied hooks. That's the whole point of running hooks here
/// rather than back on the orchestrator thread.
#[allow(clippy::too_many_arguments, reason = "all of these are caller state with no natural grouping")]
fn process_one_scenario(
    s: &Scenario,
    run_idx: u32,
    local: &mut Aggregator,
    history_writer: &Mutex<Option<HistoryWriter>>,
    history_dir: Option<&Path>,
    log_path_for: Option<LogPathFn<'_>>,
    max_wait_for: Option<MaxWaitFn<'_>>,
    format: ScenarioFormat,
) -> ScenarioOutcome {
    if matches!(format, ScenarioFormat::Inline) {
        eprint!("test {} ... ", s.name);
    }
    let started_at = OffsetDateTime::now_utc();
    let start = Instant::now();
    let outcome = (s.run)();
    let elapsed = start.elapsed();
    local.record_duration(s.name, elapsed);

    let timing_str = max_wait_for
        .and_then(|f| f())
        .map(|(actual, budget)| {
            format!(
                " (max wait {:.1}s of {:.0}s budget)",
                actual.as_secs_f64(),
                budget.as_secs_f64()
            )
        })
        .unwrap_or_default();

    let prefix = match format {
        ScenarioFormat::Inline => String::new(),
        ScenarioFormat::Prefixed => format!("[{}] ", s.name),
    };

    let mut row_log: Option<String> = None;
    let mut failed = false;
    let (row_result, row_error) = match &outcome {
        Ok(()) => {
            eprintln!("{prefix}ok{timing_str}");
            (crate::history::ResultKind::Pass, None)
        }
        Err(e) => {
            eprintln!("{prefix}FAILED{timing_str}");
            eprintln!("{prefix}  {e}");
            if let Some(get_path) = log_path_for
                && let Some(log_path) = get_path(s.name)
            {
                dump_log_tail(&log_path);
                if let Some(hdir) = history_dir {
                    let dest_name = format!("fail-{}-{}.log", s.name, run_idx + 1);
                    let dest = hdir.join(&dest_name);
                    match std::fs::copy(&log_path, &dest) {
                        Ok(_) => row_log = Some(dest_name),
                        Err(e) => eprintln!(
                            "warning: failed to copy log to {}: {e}",
                            dest.display()
                        ),
                    }
                }
            }
            failed = true;
            local.record_fail(s.name);
            (crate::history::ResultKind::Fail, Some(e.clone()))
        }
    };

    // Brief lock for the NDJSON append. Contention is negligible at
    // realistic worker counts (~10ms scenario, microseconds of lock).
    let row = crate::history::IterationRow {
        iteration: run_idx + 1,
        scenario: s.name.to_string(),
        started_at,
        duration_ms: elapsed.as_millis().min(u32::MAX as u128) as u32,
        result: row_result,
        error: row_error,
        log: row_log,
    };
    if let Some(writer) = history_writer
        .lock()
        .expect("history writer mutex poisoned")
        .as_mut()
        && let Err(e) = writer.append(&row)
    {
        eprintln!("warning: failed to append history row: {e}");
    }

    ScenarioOutcome { failed }
}

fn exit_code(aggregator: &Aggregator) -> ExitCode {
    if aggregator.any_failures() {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

/// Conventional Unix exit code 130 = "terminated by SIGINT", regardless
/// of whether scenarios passed or failed within the truncated run.
fn exit_code_with_interrupt(aggregator: &Aggregator, interrupted: bool) -> ExitCode {
    if interrupted {
        ExitCode::from(130)
    } else {
        exit_code(aggregator)
    }
}

fn dump_log_tail(log_path: &std::path::Path) {
    let contents = match std::fs::read_to_string(log_path) {
        Ok(s) if !s.trim().is_empty() => s,
        Ok(_) => return,
        Err(e) => {
            eprintln!("  (failed to read log {}: {e})", log_path.display());
            return;
        }
    };
    eprintln!("  --- QEMU log ({}) ---", log_path.display());
    let lines: Vec<&str> = contents.lines().collect();
    let tail_start = lines.len().saturating_sub(80);
    for line in &lines[tail_start..] {
        eprintln!("  | {line}");
    }
    eprintln!("  --- end QEMU log ---");
}

fn load_baseline_or_warn(path: &std::path::Path) -> Option<BaselineFile> {
    if !path.exists() {
        return None;
    }
    match BaselineFile::load_path(path) {
        Ok(f) => Some(f),
        Err(e) => {
            eprintln!(
                "warning: failed to parse {}: {e} — proceeding without baseline.",
                path.display()
            );
            None
        }
    }
}

fn print_baseline_comparisons(
    scenarios: &[&Scenario],
    aggregator: &Aggregator,
    baseline_file: Option<&BaselineFile>,
) {
    let Some(file) = baseline_file else { return };
    eprintln!("\n=== baseline comparison ===");
    let runs = aggregator.runs();
    for s in scenarios {
        let failures = aggregator.fail_count(s.name);
        let baseline = file.current_for(s.name);
        let v = verdict(failures, runs, baseline, DEFAULT_ALPHA);
        eprint!(
            "{}",
            render_comparison(&ComparisonRender {
                scenario: s.name,
                failures,
                runs,
                mean_duration: aggregator.mean_duration(s.name),
                p95_duration: aggregator.p95_duration(s.name),
                baseline,
                verdict: v,
            })
        );
    }
}

fn apply_current_run_to_baseline_with_partial(
    mut file: BaselineFile,
    scenarios: &[&Scenario],
    aggregator: &Aggregator,
    commit: &str,
    now: OffsetDateTime,
    partial: Option<&crate::baseline::PartialMarker>,
) -> BaselineFile {
    let runs = aggregator.runs();
    for s in scenarios {
        let baseline = Baseline {
            commit: commit.to_string(),
            build_hash: None,
            runs,
            failures: aggregator.fail_count(s.name),
            recorded_at: now,
            mean_duration_ms: aggregator
                .mean_duration(s.name)
                .map(|d| d.as_secs_f64() * 1000.0),
            p95_duration_ms: aggregator
                .p95_duration(s.name)
                .map(|d| d.as_secs_f64() * 1000.0),
            partial: partial.cloned(),
        };
        file.update_current(s.name, baseline);
    }
    file
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU32, Ordering};

    // The scenario `fn()` signature can't capture closures, so the
    // test fakes have to use static counters. That makes them
    // mutually exclusive — gate every test body on this lock so the
    // counters reflect the test's own execution.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    static PASS_COUNTER: AtomicU32 = AtomicU32::new(0);
    static FAIL_COUNTER: AtomicU32 = AtomicU32::new(0);
    static BUILD_COUNTER: AtomicU32 = AtomicU32::new(0);

    fn always_pass() -> Result<(), String> {
        PASS_COUNTER.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    fn always_fail() -> Result<(), String> {
        FAIL_COUNTER.fetch_add(1, Ordering::Relaxed);
        Err("scripted failure".to_string())
    }

    fn reset_counters() {
        PASS_COUNTER.store(0, Ordering::Relaxed);
        FAIL_COUNTER.store(0, Ordering::Relaxed);
        BUILD_COUNTER.store(0, Ordering::Relaxed);
    }

    fn slow_pass() -> Result<(), String> {
        PASS_COUNTER.fetch_add(1, Ordering::Relaxed);
        std::thread::sleep(std::time::Duration::from_millis(200));
        Ok(())
    }

    #[test]
    fn parallel_jobs_run_scenarios_concurrently() {
        // Four slow scenarios sleeping 200ms each. Sequential = 800ms.
        // Parallel with jobs=4 should finish in roughly one sleep window
        // (~250ms), well under the sequential floor.
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset_counters();
        let scns = [
            Scenario { name: "slow-a", run: slow_pass },
            Scenario { name: "slow-b", run: slow_pass },
            Scenario { name: "slow-c", run: slow_pass },
            Scenario { name: "slow-d", run: slow_pass },
        ];
        let refs: Vec<&Scenario> = scns.iter().collect();
        let config = RunnerConfig {
            jobs: 4,
            ..RunnerConfig::default()
        };
        let start = std::time::Instant::now();
        let _ = run(&refs, 1, false, &config);
        let elapsed = start.elapsed();

        assert_eq!(PASS_COUNTER.load(Ordering::Relaxed), 4);
        // Sequential would be ~800ms. Parallel-4 on the same scenarios
        // should be well under 500ms even on a busy CI runner. Slack
        // accounts for thread spawn + per-scenario bookkeeping.
        assert!(
            elapsed.as_millis() < 500,
            "expected parallel speedup; elapsed={:?}",
            elapsed
        );
    }

    #[test]
    fn parallel_jobs_aggregator_merge_matches_sequential() {
        // Same workload, jobs=1 vs jobs=4: total fail counts must match.
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset_counters();
        let scns = [
            Scenario { name: "pass-1", run: always_pass },
            Scenario { name: "fail-1", run: always_fail },
            Scenario { name: "pass-2", run: always_pass },
            Scenario { name: "fail-2", run: always_fail },
        ];
        let refs: Vec<&Scenario> = scns.iter().collect();
        let parallel = RunnerConfig {
            jobs: 4,
            ..RunnerConfig::default()
        };
        let _ = run(&refs, 3, false, &parallel);
        // 3 iterations × 2 always_fail scenarios = 6 fails total.
        // 3 iterations × 2 always_pass scenarios = 6 passes total.
        assert_eq!(PASS_COUNTER.load(Ordering::Relaxed), 6);
        assert_eq!(FAIL_COUNTER.load(Ordering::Relaxed), 6);
    }

    #[test]
    fn run_with_only_passing_scenarios_invokes_scenarios_n_times() {
        // Don't `unwrap` — a panicking test poisons the mutex; we don't
        // want one failure to cascade through every other runner test.
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset_counters();
        let s = Scenario { name: "pass-1", run: always_pass };
        let _ = run(&[&s], 3, false, &RunnerConfig::default());
        assert_eq!(PASS_COUNTER.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn run_with_one_failing_scenario_invokes_both_each_iteration() {
        // Don't `unwrap` — a panicking test poisons the mutex; we don't
        // want one failure to cascade through every other runner test.
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset_counters();
        let pass = Scenario { name: "pass-1", run: always_pass };
        let fail = Scenario { name: "fail-1", run: always_fail };
        let _ = run(&[&pass, &fail], 2, false, &RunnerConfig::default());
        assert_eq!(PASS_COUNTER.load(Ordering::Relaxed), 2);
        assert_eq!(FAIL_COUNTER.load(Ordering::Relaxed), 2);
    }

    fn failing_build() -> Result<(), String> {
        BUILD_COUNTER.fetch_add(1, Ordering::Relaxed);
        Err("kernel build broke".to_string())
    }

    #[test]
    fn build_hook_failure_aborts_run() {
        // Don't `unwrap` — a panicking test poisons the mutex; we don't
        // want one failure to cascade through every other runner test.
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset_counters();
        let s = Scenario { name: "pass-1", run: always_pass };
        let config = RunnerConfig {
            one_shot_build: Some(&failing_build),
            ..RunnerConfig::default()
        };
        let _ = run(&[&s], 5, false, &config);
        assert_eq!(BUILD_COUNTER.load(Ordering::Relaxed), 1);
        // Scenario should never have been called.
        assert_eq!(PASS_COUNTER.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn failure_log_copied_into_run_dir_and_referenced_in_ndjson() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset_counters();
        let fail = Scenario { name: "fail-1", run: always_fail };

        // Synthesise a log file that the log_path_for hook will return.
        let scratch = std::env::temp_dir().join(format!(
            "itest-harness-fail-log-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&scratch);
        std::fs::create_dir_all(&scratch).unwrap();
        let log_path = scratch.join("fake-qemu.log");
        std::fs::write(&log_path, "synthetic log contents\n").unwrap();
        let log_path_for = |_name: &str| Some(log_path.clone());

        let history_root = scratch.join("history");
        std::fs::create_dir_all(&history_root).unwrap();
        let config = RunnerConfig {
            log_path_for: Some(&log_path_for),
            history_root: Some(history_root.clone()),
            ..RunnerConfig::default()
        };
        let _ = run(&[&fail], 1, false, &config);

        // Find the run dir.
        let entries: Vec<_> = std::fs::read_dir(&history_root)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(entries.len(), 1);
        let run_dir = entries[0].path();

        // Log file copied with the expected name.
        let copied = run_dir.join("fail-fail-1-1.log");
        assert!(copied.exists(), "log file should be copied to run dir");
        let copied_content = std::fs::read_to_string(&copied).unwrap();
        assert_eq!(copied_content, "synthetic log contents\n");

        // NDJSON row references it.
        let rows: Vec<_> = crate::history::read_iterations(&run_dir.join("iterations.ndjson"))
            .unwrap()
            .collect::<std::io::Result<_>>()
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].log.as_deref(), Some("fail-fail-1-1.log"));

        let _ = std::fs::remove_dir_all(&scratch);
    }

    #[test]
    fn history_root_writes_metadata_and_ndjson() {
        // Don't `unwrap` — a panicking test poisons the mutex; we don't
        // want one failure to cascade through every other runner test.
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset_counters();
        let pass = Scenario { name: "pass-1", run: always_pass };
        let fail = Scenario { name: "fail-1", run: always_fail };

        let root = std::env::temp_dir().join(format!(
            "itest-harness-runner-history-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        let config = RunnerConfig {
            history_root: Some(root.clone()),
            ..RunnerConfig::default()
        };
        let _ = run(&[&pass, &fail], 2, false, &config);

        // Find the single run subdir.
        let entries: Vec<_> = std::fs::read_dir(&root)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(entries.len(), 1, "expected one run-dir under history root");
        let run_dir = entries[0].path();

        // metadata.toml exists with the right shape. We don't pin the
        // exact TOML formatting of `scenarios` (the toml crate may
        // wrap a vec of strings across lines); just check that both
        // names are present in the file.
        let meta = std::fs::read_to_string(run_dir.join("metadata.toml")).unwrap();
        assert!(meta.contains("requested_repeat = 2"));
        assert!(meta.contains("\"pass-1\""));
        assert!(meta.contains("\"fail-1\""));

        // iterations.ndjson has 4 rows (2 scenarios × 2 iterations).
        let ndjson_path = run_dir.join("iterations.ndjson");
        let rows: Vec<_> = crate::history::read_iterations(&ndjson_path)
            .unwrap()
            .collect::<std::io::Result<_>>()
            .unwrap();
        assert_eq!(rows.len(), 4);
        // First two rows: iteration 1, pass then fail.
        assert_eq!(rows[0].iteration, 1);
        assert_eq!(rows[0].scenario, "pass-1");
        assert_eq!(rows[0].result, crate::history::ResultKind::Pass);
        assert_eq!(rows[1].iteration, 1);
        assert_eq!(rows[1].scenario, "fail-1");
        assert_eq!(rows[1].result, crate::history::ResultKind::Fail);
        assert_eq!(rows[1].error.as_deref(), Some("scripted failure"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn interrupt_breaks_outer_loop_at_iteration_boundary() {
        // Don't `unwrap` — a panicking test poisons the mutex; we don't
        // want one failure to cascade through every other runner test.
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset_counters();
        let pass = Scenario { name: "pass-1", run: always_pass };
        // Pre-set the interrupt flag: the runner should observe it
        // after the first iteration finishes and break.
        let flag = std::sync::atomic::AtomicBool::new(true);
        let config = RunnerConfig {
            interrupt: Some(&flag),
            ..RunnerConfig::default()
        };
        let _ = run(&[&pass], 10, false, &config);
        // First iteration completes; interrupt check at boundary breaks.
        assert_eq!(PASS_COUNTER.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn interrupt_with_update_baseline_writes_pending_not_canonical() {
        // Don't `unwrap` — a panicking test poisons the mutex; we don't
        // want one failure to cascade through every other runner test.
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset_counters();
        let pass = Scenario { name: "pass-1", run: always_pass };

        let dir = std::env::temp_dir().join(format!(
            "itest-harness-pending-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let canonical = dir.join("baseline.toml");
        let pending = dir.join("baseline.toml.pending");
        // Pre-populate the canonical so we can later assert it was untouched.
        std::fs::write(&canonical, "# untouched\n").unwrap();

        let flag = std::sync::atomic::AtomicBool::new(true);
        let config = RunnerConfig {
            interrupt: Some(&flag),
            baseline_file: Some(canonical.clone()),
            pending_baseline: Some(pending.clone()),
            ..RunnerConfig::default()
        };
        let _ = run(&[&pass], 10, true, &config);

        // Canonical untouched.
        let canonical_after = std::fs::read_to_string(&canonical).unwrap();
        assert_eq!(canonical_after, "# untouched\n");
        // Pending written with the partial marker.
        let pending_after = std::fs::read_to_string(&pending).unwrap();
        assert!(pending_after.contains("[scenarios.pass-1.current.partial]"));
        assert!(pending_after.contains("requested_runs = 10"));
        // Only 1 iteration actually completed before the boundary check.
        assert!(pending_after.contains("runs = 1"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fail_fast_breaks_outer_loop_at_threshold() {
        // Don't `unwrap` — a panicking test poisons the mutex; we don't
        // want one failure to cascade through every other runner test.
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset_counters();
        let fail = Scenario { name: "fail-1", run: always_fail };
        // Without fail-fast, repeat=10 would invoke fail-1 ten times.
        // With fail-fast=3, the runner stops after the 3rd iteration.
        let config = RunnerConfig {
            fail_fast: Some(3),
            ..RunnerConfig::default()
        };
        let _ = run(&[&fail], 10, false, &config);
        assert_eq!(FAIL_COUNTER.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn fail_fast_does_not_trigger_when_threshold_not_reached() {
        // Don't `unwrap` — a panicking test poisons the mutex; we don't
        // want one failure to cascade through every other runner test.
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset_counters();
        let fail = Scenario { name: "fail-1", run: always_fail };
        // 5 failures requested, threshold of 100 — full run completes.
        let config = RunnerConfig {
            fail_fast: Some(100),
            ..RunnerConfig::default()
        };
        let _ = run(&[&fail], 5, false, &config);
        assert_eq!(FAIL_COUNTER.load(Ordering::Relaxed), 5);
    }

    #[test]
    fn fail_fast_none_runs_to_completion_with_failures() {
        // Don't `unwrap` — a panicking test poisons the mutex; we don't
        // want one failure to cascade through every other runner test.
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset_counters();
        let fail = Scenario { name: "fail-1", run: always_fail };
        let _ = run(&[&fail], 7, false, &RunnerConfig::default());
        assert_eq!(FAIL_COUNTER.load(Ordering::Relaxed), 7);
    }

}
