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

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use time::OffsetDateTime;

use crate::aggregate::{Aggregator, RunTotals};
use crate::baseline::{Baseline, BaselineFile};
use crate::verdict::{ComparisonRender, DEFAULT_ALPHA, render_comparison, verdict};

/// One scenario: a name + a function that returns `Ok(())` on pass or
/// a human-readable error string on failure. Same shape xtask has
/// used since v0.1.
pub struct Scenario {
    pub name: &'static str,
    pub run: fn() -> Result<(), String>,
}

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
    pub log_path_for: Option<&'a dyn Fn(&str) -> Option<PathBuf>>,

    /// Called after each scenario; returns `(actual_elapsed, budget)`
    /// from whatever the consumer's harness measured (NOT the same as
    /// the runner's wall-clock timing — this is e.g. virtio-frame wait
    /// budget exposure). Used only for inline display.
    pub max_wait_for: Option<&'a dyn Fn() -> Option<(Duration, Duration)>>,

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
    let now_for_history = OffsetDateTime::now_utc();
    let mut history_writer: Option<crate::history::HistoryWriter> = None;
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
                history_writer = Some(writer);
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

    for run_idx in 0..runs {
        if runs > 1 {
            eprintln!("\n=== run {}/{} ===", run_idx + 1, runs);
        }
        // Step 2 of plans/itest-parallel-scenarios.md: each iteration
        // populates a local `Aggregator`, which is then merged into the
        // master. Behaviour-equivalent at `--jobs 1`; step 3 swaps the
        // sequential scenario loop for a worker pool that produces
        // per-worker aggregators reduced via the same `merge`.
        let mut local = Aggregator::new();
        let mut failed_this_run = 0;
        for s in scenarios {
            eprint!("test {} ... ", s.name);
            let started_at = OffsetDateTime::now_utc();
            let start = Instant::now();
            let outcome = (s.run)();
            let elapsed = start.elapsed();
            local.record_duration(s.name, elapsed);

            let timing_str = config
                .max_wait_for
                .and_then(|f| f())
                .map(|(actual, budget)| {
                    format!(
                        " (max wait {:.1}s of {:.0}s budget)",
                        actual.as_secs_f64(),
                        budget.as_secs_f64()
                    )
                })
                .unwrap_or_default();

            let mut row_log: Option<String> = None;
            let (row_result, row_error) = match &outcome {
                Ok(()) => {
                    eprintln!("ok{timing_str}");
                    (crate::history::ResultKind::Pass, None)
                }
                Err(e) => {
                    eprintln!("FAILED{timing_str}");
                    eprintln!("  {e}");
                    if let Some(get_path) = config.log_path_for
                        && let Some(log_path) = get_path(s.name)
                    {
                        dump_log_tail(&log_path);
                        // Tier 3: persist the per-failure log into the
                        // run directory. The NDJSON row's `log` field
                        // gets the filename so a future viewer can
                        // open it directly from the run dir.
                        if let Some(hdir) = history_dir.as_ref() {
                            let dest_name =
                                format!("fail-{}-{}.log", s.name, run_idx + 1);
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
                    failed_this_run += 1;
                    local.record_fail(s.name);
                    (crate::history::ResultKind::Fail, Some(e.clone()))
                }
            };

            if let Some(writer) = history_writer.as_mut() {
                let row = crate::history::IterationRow {
                    iteration: run_idx + 1,
                    scenario: s.name.to_string(),
                    started_at,
                    duration_ms: elapsed.as_millis().min(u32::MAX as u128) as u32,
                    result: row_result,
                    error: row_error,
                    log: row_log,
                };
                if let Err(e) = writer.append(&row) {
                    eprintln!("warning: failed to append history row: {e}");
                }
            }
        }
        let total = scenarios.len();
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
