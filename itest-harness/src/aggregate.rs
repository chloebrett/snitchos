//! Per-scenario flake aggregation across `--repeat N` runs.
//!
//! The runner drives this incrementally: after each scenario, the caller
//! calls `record_pass`/`record_fail`. At the end of each run, the caller
//! calls `finish_run` to capture per-run totals. After all runs, the
//! caller calls `render_aggregate` for the printable summary.
//!
//! Owning printing here would couple the harness to stdout/stderr; we
//! return a rendered string instead so the caller decides where it goes
//! and tests can assert on the result.

use std::collections::BTreeMap;
use std::time::Duration;

/// Pass/fail counts for a single `--repeat` iteration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunTotals {
    pub passed: usize,
    pub failed: usize,
}

/// Accumulates per-scenario fail counts and per-run totals across an
/// entire `--repeat N` execution.
///
/// `BTreeMap` keeps the flake table in lexicographic order, which is
/// stable across runs and trivially comparable in tests. (Previously
/// keyed by `&'static str` for "scenario registration order"; that
/// coupled the report to the consumer's static lifetime. Lexicographic
/// is good enough and decoupling is the point.)
#[derive(Debug, Default, Clone)]
pub struct Aggregator {
    fail_count: BTreeMap<String, u32>,
    run_totals: Vec<RunTotals>,
    /// Per-scenario, per-iteration wall-clock durations. Appended in
    /// observation order; sorted for percentile computation.
    durations: BTreeMap<String, Vec<Duration>>,
}

impl Aggregator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one scenario failure during the current run.
    pub fn record_fail(&mut self, scenario: &str) {
        // `entry` + `or_insert(0)` mirrors xtask's prior implementation;
        // the count is total failures for this scenario across all runs.
        *self.fail_count.entry(scenario.to_string()).or_insert(0) += 1;
    }

    /// Capture this run's totals. Call once per `--repeat` iteration
    /// after every scenario has been recorded.
    pub fn finish_run(&mut self, totals: RunTotals) {
        self.run_totals.push(totals);
    }

    /// Did any scenario fail in any run?
    pub fn any_failures(&self) -> bool {
        !self.fail_count.is_empty()
    }

    /// Per-run pass/fail totals, in run order.
    pub fn run_totals(&self) -> &[RunTotals] {
        &self.run_totals
    }

    /// Scenarios that failed at least once, with their cumulative
    /// fail count across all runs. Iterated in lexicographic order.
    pub fn flaky(&self) -> impl Iterator<Item = (&str, u32)> + '_ {
        self.fail_count.iter().map(|(name, count)| (name.as_str(), *count))
    }

    /// Cumulative fail count for a single scenario across all runs.
    /// Returns 0 for scenarios that never failed (or never ran).
    pub fn fail_count(&self, scenario: &str) -> u32 {
        self.fail_count.get(scenario).copied().unwrap_or(0)
    }

    /// Total number of `--repeat` iterations recorded so far.
    pub fn runs(&self) -> u32 {
        self.run_totals.len() as u32
    }

    /// Record a wall-clock duration observation for a scenario in this
    /// run. Call once per scenario per `--repeat` iteration.
    pub fn record_duration(&mut self, scenario: &str, duration: Duration) {
        self.durations
            .entry(scenario.to_string())
            .or_default()
            .push(duration);
    }

    /// Mean of the recorded durations for `scenario`. `None` if no
    /// observations exist (scenario never ran, or timing was never
    /// recorded).
    pub fn mean_duration(&self, scenario: &str) -> Option<Duration> {
        let v = self.durations.get(scenario)?;
        if v.is_empty() {
            return None;
        }
        let total: Duration = v.iter().sum();
        Some(total / v.len() as u32)
    }

    /// p95 of the recorded durations via the nearest-rank method:
    /// sort the samples and pick the value at `ceil(0.95 * n) - 1`.
    /// `None` if no observations.
    pub fn p95_duration(&self, scenario: &str) -> Option<Duration> {
        self.percentile_duration(scenario, 0.95)
    }

    fn percentile_duration(&self, scenario: &str, p: f64) -> Option<Duration> {
        let v = self.durations.get(scenario)?;
        if v.is_empty() {
            return None;
        }
        let n = v.len();
        // Sort ascending. We work on a copy so the recorded order is
        // preserved (debugging / future serialisation).
        let mut sorted = v.clone();
        sorted.sort();
        let rank = (p * n as f64).ceil() as usize;
        // Clamp 1..=n then convert to 0..=n-1 index.
        let idx = rank.max(1).min(n) - 1;
        Some(sorted[idx])
    }

    /// Render the multi-run aggregate summary. Format matches xtask's
    /// pre-extraction output so a side-by-side diff during migration is
    /// zero lines.
    pub fn render_aggregate(&self, total_runs: u32) -> String {
        use std::fmt::Write;
        let mut out = String::new();
        let _ = writeln!(out, "\n=== aggregate over {total_runs} runs ===");
        for (i, totals) in self.run_totals.iter().enumerate() {
            let _ = writeln!(
                out,
                "  run {}: {} passed, {} failed",
                i + 1,
                totals.passed,
                totals.failed
            );
        }
        if self.fail_count.is_empty() {
            let _ = writeln!(out, "\nNo flakes — every scenario passed every run.");
        } else {
            let _ = writeln!(out, "\nFlaky scenarios (failed at least once):");
            for (name, count) in &self.fail_count {
                let _ = writeln!(out, "  {name}: {count}/{total_runs} runs failed");
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_aggregator_reports_no_failures() {
        let agg = Aggregator::new();
        assert!(!agg.any_failures());
        assert_eq!(agg.run_totals().len(), 0);
        assert_eq!(agg.flaky().count(), 0);
    }

    #[test]
    fn single_failure_recorded() {
        let mut agg = Aggregator::new();
        agg.record_fail("heartbeat-cadence");
        agg.finish_run(RunTotals { passed: 17, failed: 1 });

        assert!(agg.any_failures());
        let flaky: Vec<_> = agg.flaky().collect();
        assert_eq!(flaky, vec![("heartbeat-cadence", 1)]);
        assert_eq!(
            agg.run_totals(),
            &[RunTotals { passed: 17, failed: 1 }]
        );
    }

    #[test]
    fn same_scenario_failing_twice_accumulates() {
        let mut agg = Aggregator::new();
        agg.record_fail("heartbeat-cadence");
        agg.finish_run(RunTotals { passed: 17, failed: 1 });
        agg.record_fail("heartbeat-cadence");
        agg.finish_run(RunTotals { passed: 17, failed: 1 });

        let flaky: Vec<_> = agg.flaky().collect();
        assert_eq!(flaky, vec![("heartbeat-cadence", 2)]);
    }

    #[test]
    fn flaky_sorted_lexicographically() {
        let mut agg = Aggregator::new();
        agg.record_fail("zzz-scenario");
        agg.record_fail("aaa-scenario");
        agg.record_fail("mmm-scenario");
        agg.finish_run(RunTotals { passed: 0, failed: 3 });

        let names: Vec<_> = agg.flaky().map(|(n, _)| n).collect();
        assert_eq!(names, vec!["aaa-scenario", "mmm-scenario", "zzz-scenario"]);
    }

    #[test]
    fn render_no_flakes() {
        let mut agg = Aggregator::new();
        agg.finish_run(RunTotals { passed: 18, failed: 0 });
        agg.finish_run(RunTotals { passed: 18, failed: 0 });

        let out = agg.render_aggregate(2);
        assert!(out.contains("=== aggregate over 2 runs ==="));
        assert!(out.contains("run 1: 18 passed, 0 failed"));
        assert!(out.contains("run 2: 18 passed, 0 failed"));
        assert!(out.contains("No flakes"));
        assert!(!out.contains("Flaky scenarios"));
    }

    #[test]
    fn fail_count_returns_zero_for_unrecorded_scenarios() {
        let mut agg = Aggregator::new();
        agg.record_fail("heartbeat-cadence");
        agg.finish_run(RunTotals { passed: 17, failed: 1 });
        assert_eq!(agg.fail_count("heartbeat-cadence"), 1);
        assert_eq!(agg.fail_count("never-ran"), 0);
        assert_eq!(agg.runs(), 1);
    }

    #[test]
    fn mean_duration_returns_average_of_recorded_observations() {
        let mut agg = Aggregator::new();
        agg.record_duration("heartbeat-cadence", Duration::from_millis(100));
        agg.record_duration("heartbeat-cadence", Duration::from_millis(200));
        agg.record_duration("heartbeat-cadence", Duration::from_millis(300));
        assert_eq!(
            agg.mean_duration("heartbeat-cadence"),
            Some(Duration::from_millis(200))
        );
    }

    #[test]
    fn mean_duration_none_for_unrecorded_scenarios() {
        let agg = Aggregator::new();
        assert_eq!(agg.mean_duration("never-ran"), None);
    }

    #[test]
    fn p95_duration_nearest_rank() {
        let mut agg = Aggregator::new();
        // 100 samples: 1ms, 2ms, ..., 100ms. p95 = 95th sample = 95ms.
        for i in 1..=100u64 {
            agg.record_duration("scn", Duration::from_millis(i));
        }
        assert_eq!(agg.p95_duration("scn"), Some(Duration::from_millis(95)));
    }

    #[test]
    fn p95_duration_small_sample_returns_max() {
        let mut agg = Aggregator::new();
        agg.record_duration("scn", Duration::from_millis(10));
        agg.record_duration("scn", Duration::from_millis(20));
        agg.record_duration("scn", Duration::from_millis(30));
        // ceil(0.95 * 3) = 3; index 2 = 30ms.
        assert_eq!(agg.p95_duration("scn"), Some(Duration::from_millis(30)));
    }

    #[test]
    fn duration_observations_unaffected_by_insertion_order() {
        // p95 sorts internally; a "slow first" sequence produces the
        // same answer as "slow last."
        let mut a = Aggregator::new();
        let mut b = Aggregator::new();
        for ms in [50, 10, 30, 20, 100u64] {
            a.record_duration("scn", Duration::from_millis(ms));
        }
        for ms in [10, 20, 30, 50, 100u64] {
            b.record_duration("scn", Duration::from_millis(ms));
        }
        assert_eq!(a.p95_duration("scn"), b.p95_duration("scn"));
        assert_eq!(a.mean_duration("scn"), b.mean_duration("scn"));
    }

    #[test]
    fn render_with_flakes() {
        let mut agg = Aggregator::new();
        agg.record_fail("heartbeat-cadence");
        agg.finish_run(RunTotals { passed: 17, failed: 1 });
        agg.finish_run(RunTotals { passed: 18, failed: 0 });
        agg.record_fail("kernel-heap-metrics");
        agg.record_fail("heartbeat-cadence");
        agg.finish_run(RunTotals { passed: 16, failed: 2 });

        let out = agg.render_aggregate(3);
        assert!(out.contains("Flaky scenarios"));
        assert!(out.contains("heartbeat-cadence: 2/3 runs failed"));
        assert!(out.contains("kernel-heap-metrics: 1/3 runs failed"));
        assert!(!out.contains("No flakes"));
    }
}
