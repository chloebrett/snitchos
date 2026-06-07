//! Generic `--repeat N` runner. Owns the per-iteration loop, scenario
//! invocation, aggregation, baseline I/O, and the printable output —
//! everything that doesn't depend on what's under test.
//!
//! The consumer (xtask, for now) plugs in:
//!
//! - The scenario list (`&[Scenario]`).
//! - Hooks: `kill_stale`, `one_shot_build`, `log_path_for`,
//!   `max_wait_for`, `current_commit`. All optional; the runner
//!   no-ops when a hook is `None`.
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
    /// Called once before the first scenario. Typical use:
    /// `pkill qemu-system-riscv64` to clear stale subjects.
    pub kill_stale: Option<&'a dyn Fn()>,

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
    if let Some(f) = config.kill_stale {
        f();
    }
    if let Some(build) = config.one_shot_build
        && let Err(e) = build()
    {
        eprintln!("kernel build failed: {e}");
        return ExitCode::from(2);
    }

    let runs = repeat.max(1);
    let mut aggregator = Aggregator::new();

    for run_idx in 0..runs {
        if runs > 1 {
            eprintln!("\n=== run {}/{} ===", run_idx + 1, runs);
        }
        let mut failed_this_run = 0;
        for s in scenarios {
            eprint!("test {} ... ", s.name);
            let start = Instant::now();
            let outcome = (s.run)();
            aggregator.record_duration(s.name, start.elapsed());

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

            match outcome {
                Ok(()) => eprintln!("ok{timing_str}"),
                Err(e) => {
                    eprintln!("FAILED{timing_str}");
                    eprintln!("  {e}");
                    if let Some(get_path) = config.log_path_for
                        && let Some(log_path) = get_path(s.name)
                    {
                        dump_log_tail(&log_path);
                    }
                    failed_this_run += 1;
                    aggregator.record_fail(s.name);
                }
            }
        }
        let total = scenarios.len();
        eprintln!("\n{} passed, {} failed", total - failed_this_run, failed_this_run);
        aggregator.finish_run(RunTotals {
            passed: total - failed_this_run,
            failed: failed_this_run,
        });

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
        let Some(path) = config.baseline_file.as_deref() else {
            eprintln!("warning: --update-baseline requested but no baseline_file path was supplied");
            return exit_code(&aggregator);
        };
        let commit = config
            .current_commit
            .and_then(|f| f())
            .unwrap_or_else(|| "unknown".to_string());
        let now = OffsetDateTime::now_utc();
        let updated = apply_current_run_to_baseline(
            baseline_file.unwrap_or_default(),
            scenarios,
            &aggregator,
            &commit,
            now,
        );
        if let Err(e) = updated.save_path(path) {
            eprintln!("warning: failed to write {}: {e}", path.display());
        } else {
            eprintln!("\nUpdated {} with current run's results.", path.display());
        }
    }

    exit_code(&aggregator)
}

fn exit_code(aggregator: &Aggregator) -> ExitCode {
    if aggregator.any_failures() {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
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

fn apply_current_run_to_baseline(
    mut file: BaselineFile,
    scenarios: &[&Scenario],
    aggregator: &Aggregator,
    commit: &str,
    now: OffsetDateTime,
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
    static KILL_COUNTER: AtomicU32 = AtomicU32::new(0);
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
        KILL_COUNTER.store(0, Ordering::Relaxed);
        BUILD_COUNTER.store(0, Ordering::Relaxed);
    }

    #[test]
    fn run_with_only_passing_scenarios_invokes_scenarios_n_times() {
        let _guard = TEST_LOCK.lock().unwrap();
        reset_counters();
        let s = Scenario { name: "pass-1", run: always_pass };
        let _ = run(&[&s], 3, false, &RunnerConfig::default());
        assert_eq!(PASS_COUNTER.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn run_with_one_failing_scenario_invokes_both_each_iteration() {
        let _guard = TEST_LOCK.lock().unwrap();
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
        let _guard = TEST_LOCK.lock().unwrap();
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

    fn count_kill() {
        KILL_COUNTER.fetch_add(1, Ordering::Relaxed);
    }

    #[test]
    fn fail_fast_breaks_outer_loop_at_threshold() {
        let _guard = TEST_LOCK.lock().unwrap();
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
        let _guard = TEST_LOCK.lock().unwrap();
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
        let _guard = TEST_LOCK.lock().unwrap();
        reset_counters();
        let fail = Scenario { name: "fail-1", run: always_fail };
        let _ = run(&[&fail], 7, false, &RunnerConfig::default());
        assert_eq!(FAIL_COUNTER.load(Ordering::Relaxed), 7);
    }

    #[test]
    fn kill_stale_hook_invoked_once_before_scenarios() {
        let _guard = TEST_LOCK.lock().unwrap();
        reset_counters();
        let s = Scenario { name: "pass-1", run: always_pass };
        let config = RunnerConfig {
            kill_stale: Some(&count_kill),
            ..RunnerConfig::default()
        };
        let _ = run(&[&s], 3, false, &config);
        assert_eq!(KILL_COUNTER.load(Ordering::Relaxed), 1);
        assert_eq!(PASS_COUNTER.load(Ordering::Relaxed), 3);
    }
}
