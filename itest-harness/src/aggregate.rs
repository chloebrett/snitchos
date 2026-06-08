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

use crate::signature::Signature;

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
    /// Per-scenario count of failures by cause-bucket. Failures with no
    /// recorded signature count as `Unknown`. The suite-wide breakdown
    /// is folded from this on demand.
    signature_by_scenario: BTreeMap<String, BTreeMap<Signature, u32>>,
}

impl Aggregator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Test-only ergonomic recorder: an untyped failure (no cause-bucket),
    /// equivalent to `record_fail_with_signature(_, None)`. Production
    /// always records *with* a signature, so this exists only to keep the
    /// aggregate tests terse.
    #[cfg(test)]
    pub fn record_fail(&mut self, scenario: &str) {
        self.record_fail_with_signature(scenario, None);
    }

    /// Record one scenario failure with its classified cause-bucket. A
    /// `None` signature counts toward `Unknown` so every failure is
    /// represented in the breakdown.
    pub fn record_fail_with_signature(&mut self, scenario: &str, signature: Option<Signature>) {
        // `entry` + `or_insert(0)` mirrors xtask's prior implementation;
        // the count is total failures for this scenario across all runs.
        *self.fail_count.entry(scenario.to_string()).or_insert(0) += 1;
        *self
            .signature_by_scenario
            .entry(scenario.to_string())
            .or_default()
            .entry(signature.unwrap_or(Signature::Unknown))
            .or_insert(0) += 1;
    }

    /// Suite-wide failure counts by cause-bucket, folded across every
    /// scenario. Empty when there were no failures.
    pub fn signature_counts(&self) -> BTreeMap<Signature, u32> {
        let mut folded: BTreeMap<Signature, u32> = BTreeMap::new();
        for per_scenario in self.signature_by_scenario.values() {
            for (sig, count) in per_scenario {
                *folded.entry(*sig).or_insert(0) += count;
            }
        }
        folded
    }

    /// Failure counts by cause-bucket for one scenario. Empty for a
    /// scenario that never failed.
    pub fn signature_counts_for(&self, scenario: &str) -> BTreeMap<Signature, u32> {
        self.signature_by_scenario
            .get(scenario)
            .cloned()
            .unwrap_or_default()
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

    /// Scenarios that failed at least once, with their cumulative fail
    /// count across all runs, in lexicographic order. Test-only accessor:
    /// production renders from `fail_count` / `signature_counts` directly.
    #[cfg(test)]
    pub fn flaky(&self) -> impl Iterator<Item = (&str, u32)> + '_ {
        self.fail_count.iter().map(|(name, count)| (name.as_str(), *count))
    }

    /// Cumulative fail count for a single scenario across all runs.
    /// Returns 0 for scenarios that never failed (or never ran).
    pub fn fail_count(&self, scenario: &str) -> u32 {
        self.fail_count.get(scenario).copied().unwrap_or(0)
    }

    /// Total failures across all scenarios and all runs recorded so
    /// far. Used by the runner's `--fail-fast=K` early-exit check.
    pub fn total_failures(&self) -> u32 {
        self.fail_count.values().sum()
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

    /// Sum of every recorded scenario duration across every scenario.
    /// In a per-iteration aggregator this is the iteration's total
    /// CPU time (each scenario consumes ~one host core for its
    /// duration). Compare against wall-clock to see the parallelism
    /// factor.
    pub fn total_duration(&self) -> Duration {
        self.durations.values().flatten().sum()
    }

    /// Fold `other` into `self`. Used by the parallel runner to
    /// gather per-worker (or per-iteration) aggregators back into a
    /// single suite-level view at the end of a run. Semantics:
    ///
    /// - `fail_count`: per-scenario pointwise sum.
    /// - `run_totals`: append `other`'s after `self`'s. Caller is
    ///   responsible for the ordering it wants — chronological,
    ///   worker-grouped, etc.
    /// - `durations`: per-scenario, concat `other`'s observations
    ///   after `self`'s. Mean / p95 are computed lazily over the
    ///   merged vector, so observation order across the merge is
    ///   irrelevant.
    pub fn merge(&mut self, other: Aggregator) {
        for (name, count) in other.fail_count {
            *self.fail_count.entry(name).or_insert(0) += count;
        }
        self.run_totals.extend(other.run_totals);
        for (name, mut samples) in other.durations {
            self.durations
                .entry(name)
                .or_default()
                .append(&mut samples);
        }
        for (scenario, per_scenario) in other.signature_by_scenario {
            let dst = self.signature_by_scenario.entry(scenario).or_default();
            for (sig, count) in per_scenario {
                *dst.entry(sig).or_insert(0) += count;
            }
        }
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
            out.push_str(&self.render_signature_breakdown());
        }
        out
    }

    /// Render the suite-wide failure-by-bucket breakdown, or the empty
    /// string when nothing failed. Shared by `render_aggregate` (the
    /// `--repeat` summary) and the single-run failure path, so a one-off
    /// run that flakes still reports which bucket it hit.
    pub fn render_signature_breakdown(&self) -> String {
        use std::fmt::Write;
        let folded = self.signature_counts();
        if folded.is_empty() {
            return String::new();
        }
        let mut out = String::new();
        let total: u32 = folded.values().sum();
        let _ = writeln!(out, "\nFailure signatures ({total} total):");
        for (sig, count) in &folded {
            let _ = writeln!(out, "  {}: {count}", sig.label());
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
    fn total_failures_sums_across_scenarios_and_runs() {
        let mut agg = Aggregator::new();
        agg.record_fail("a");
        agg.record_fail("b");
        agg.finish_run(RunTotals { passed: 0, failed: 2 });
        agg.record_fail("a");
        agg.finish_run(RunTotals { passed: 0, failed: 1 });
        assert_eq!(agg.total_failures(), 3);
    }

    #[test]
    fn total_failures_zero_when_no_failures() {
        let agg = Aggregator::new();
        assert_eq!(agg.total_failures(), 0);
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
    fn total_duration_sums_across_all_scenarios() {
        let mut agg = Aggregator::new();
        agg.record_duration("a", Duration::from_millis(100));
        agg.record_duration("a", Duration::from_millis(200));
        agg.record_duration("b", Duration::from_millis(50));
        assert_eq!(agg.total_duration(), Duration::from_millis(350));
    }

    #[test]
    fn total_duration_zero_when_no_observations() {
        let agg = Aggregator::new();
        assert_eq!(agg.total_duration(), Duration::ZERO);
    }

    #[test]
    fn merge_into_empty_yields_same_data() {
        let mut a = Aggregator::new();
        let mut b = Aggregator::new();
        b.record_fail("heartbeat-cadence");
        b.record_duration("heartbeat-cadence", Duration::from_millis(120));
        b.finish_run(RunTotals { passed: 17, failed: 1 });

        a.merge(b);

        assert_eq!(a.fail_count("heartbeat-cadence"), 1);
        assert_eq!(a.run_totals(), &[RunTotals { passed: 17, failed: 1 }]);
        assert_eq!(
            a.mean_duration("heartbeat-cadence"),
            Some(Duration::from_millis(120))
        );
    }

    #[test]
    fn merge_sums_per_scenario_fail_counts() {
        let mut a = Aggregator::new();
        let mut b = Aggregator::new();
        // a saw heartbeat-cadence fail twice, kernel-heap-metrics once.
        a.record_fail("heartbeat-cadence");
        a.record_fail("heartbeat-cadence");
        a.record_fail("kernel-heap-metrics");
        // b saw heartbeat-cadence fail once, frame-allocator-metrics once.
        b.record_fail("heartbeat-cadence");
        b.record_fail("frame-allocator-metrics");

        a.merge(b);

        assert_eq!(a.fail_count("heartbeat-cadence"), 3);
        assert_eq!(a.fail_count("kernel-heap-metrics"), 1);
        assert_eq!(a.fail_count("frame-allocator-metrics"), 1);
        assert_eq!(a.total_failures(), 5);
    }

    #[test]
    fn merge_appends_run_totals_in_order() {
        let mut a = Aggregator::new();
        let mut b = Aggregator::new();
        a.finish_run(RunTotals { passed: 18, failed: 0 });
        a.finish_run(RunTotals { passed: 17, failed: 1 });
        b.finish_run(RunTotals { passed: 16, failed: 2 });
        b.finish_run(RunTotals { passed: 15, failed: 3 });

        a.merge(b);

        assert_eq!(
            a.run_totals(),
            &[
                RunTotals { passed: 18, failed: 0 },
                RunTotals { passed: 17, failed: 1 },
                RunTotals { passed: 16, failed: 2 },
                RunTotals { passed: 15, failed: 3 },
            ]
        );
        assert_eq!(a.runs(), 4);
    }

    #[test]
    fn merge_concats_per_scenario_durations() {
        // Equivalence check: split observations across two aggregators
        // and merge, vs record all into one. Statistics should match.
        let mut split_a = Aggregator::new();
        let mut split_b = Aggregator::new();
        let mut whole = Aggregator::new();
        for ms in [50, 10, 30u64] {
            split_a.record_duration("scn", Duration::from_millis(ms));
        }
        for ms in [20, 100u64] {
            split_b.record_duration("scn", Duration::from_millis(ms));
        }
        for ms in [50, 10, 30, 20, 100u64] {
            whole.record_duration("scn", Duration::from_millis(ms));
        }
        split_a.merge(split_b);

        assert_eq!(split_a.mean_duration("scn"), whole.mean_duration("scn"));
        assert_eq!(split_a.p95_duration("scn"), whole.p95_duration("scn"));
    }

    #[test]
    fn merge_is_associative_on_typical_workload() {
        // (a ⊕ b) ⊕ c should equal a ⊕ (b ⊕ c) for the operations we
        // do. Important for step 3: workers' aggregators get merged
        // pairwise; the result must not depend on the merge tree.
        let make = |fails: &[&str], totals: &[(usize, usize)], durs: &[(u64,)]| {
            let mut x = Aggregator::new();
            for f in fails {
                x.record_fail(f);
            }
            for &(p, f) in totals {
                x.finish_run(RunTotals { passed: p, failed: f });
            }
            for &(ms,) in durs {
                x.record_duration("scn", Duration::from_millis(ms));
            }
            x
        };
        let left_first = {
            let mut a = make(&["x", "y"], &[(10, 1)], &[(100,)]);
            let b = make(&["x"], &[(11, 0)], &[(50,)]);
            let c = make(&["z"], &[(9, 2)], &[(200,)]);
            a.merge(b);
            a.merge(c);
            a
        };
        let right_first = {
            let a = make(&["x", "y"], &[(10, 1)], &[(100,)]);
            let mut bc = make(&["x"], &[(11, 0)], &[(50,)]);
            let c = make(&["z"], &[(9, 2)], &[(200,)]);
            bc.merge(c);
            let mut all = a;
            all.merge(bc);
            all
        };
        assert_eq!(left_first.fail_count("x"), right_first.fail_count("x"));
        assert_eq!(left_first.fail_count("y"), right_first.fail_count("y"));
        assert_eq!(left_first.fail_count("z"), right_first.fail_count("z"));
        assert_eq!(left_first.run_totals(), right_first.run_totals());
        assert_eq!(
            left_first.mean_duration("scn"),
            right_first.mean_duration("scn")
        );
    }

    #[test]
    fn signature_counts_are_tracked_per_scenario() {
        use crate::signature::Signature;
        let mut agg = Aggregator::new();
        agg.record_fail_with_signature("scn-a", Some(Signature::Wedge));
        agg.record_fail_with_signature("scn-a", Some(Signature::BudgetExhausted));
        agg.record_fail_with_signature("scn-b", Some(Signature::Wedge));

        let a = agg.signature_counts_for("scn-a");
        assert_eq!(a.get(&Signature::Wedge), Some(&1));
        assert_eq!(a.get(&Signature::BudgetExhausted), Some(&1));
        let b = agg.signature_counts_for("scn-b");
        assert_eq!(b.get(&Signature::Wedge), Some(&1));
        assert!(agg.signature_counts_for("never-failed").is_empty());

        // The suite-wide fold still works (drives the terminal summary).
        assert_eq!(agg.signature_counts().get(&Signature::Wedge), Some(&2));
        assert_eq!(agg.signature_counts().get(&Signature::BudgetExhausted), Some(&1));
    }

    #[test]
    fn signature_counts_accumulate_merge_and_render() {
        use crate::signature::Signature;
        let mut a = Aggregator::new();
        a.record_fail_with_signature("scn-a", Some(Signature::Wedge));
        a.record_fail_with_signature("scn-b", Some(Signature::BudgetExhausted));
        let mut b = Aggregator::new();
        b.record_fail_with_signature("scn-c", Some(Signature::Wedge));
        b.record_fail_with_signature("scn-d", None); // unclassified — counted as Unknown
        a.merge(b);
        a.finish_run(RunTotals { passed: 0, failed: 4 });

        let counts = a.signature_counts();
        assert_eq!(counts.get(&Signature::Wedge), Some(&2));
        assert_eq!(counts.get(&Signature::BudgetExhausted), Some(&1));
        assert_eq!(counts.get(&Signature::Unknown), Some(&1));

        let out = a.render_aggregate(1);
        assert!(out.contains("Failure signatures"));
        assert!(out.contains("wedge: 2"));
        assert!(out.contains("budget_exhausted: 1"));
        assert!(out.contains("unknown: 1"));
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
