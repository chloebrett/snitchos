//! Compare a current `--repeat` run against a stored baseline and emit
//! a verdict. Glue between `stats` and `baseline`.
//!
//! Threshold convention: `p_value < alpha` ⇒ `Different`, else
//! `Consistent`. `alpha = 0.05` is the default; the runner exposes it
//! as a knob if a tighter or looser gate is needed.
//!
//! The render helper produces a multi-line string per scenario in the
//! aggregate-section format the harness will eventually print. Kept
//! here (not in `baseline.rs`) because rendering needs stats too.

use std::fmt::Write;

use crate::baseline::Baseline;
use crate::stats::{two_proportion_p_value, wilson_score_95};

/// Default p-value threshold for declaring rates different. Conventional
/// 5% two-sided gate.
pub const DEFAULT_ALPHA: f64 = 0.05;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Verdict {
    /// No baseline stored for this scenario yet — caller has nothing
    /// to compare to.
    NoBaseline,
    /// Current run is statistically consistent with baseline at the
    /// chosen alpha.
    Consistent { p_value: f64 },
    /// Current run differs from baseline at the chosen alpha. Sign
    /// indicates direction.
    Different {
        p_value: f64,
        direction: Direction,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Direction {
    /// Current rate is higher than baseline (regression).
    Worse,
    /// Current rate is lower than baseline (improvement).
    Better,
    /// Same observed rate but enough samples differ that the p-value
    /// crossed alpha. Rare; surfaces sample-size mismatches.
    Same,
}

/// Decide whether `(failures, runs)` is consistent with `baseline`.
/// Pass `DEFAULT_ALPHA` for the conventional 0.05 gate.
pub fn verdict(
    failures: u32,
    runs: u32,
    baseline: Option<&Baseline>,
    alpha: f64,
) -> Verdict {
    let Some(b) = baseline else {
        return Verdict::NoBaseline;
    };
    let p = two_proportion_p_value(failures, runs, b.failures, b.runs);
    if p >= alpha {
        Verdict::Consistent { p_value: p }
    } else {
        let cur_rate = if runs == 0 { 0.0 } else { f64::from(failures) / f64::from(runs) };
        let direction = match cur_rate.partial_cmp(&b.rate()) {
            Some(std::cmp::Ordering::Greater) => Direction::Worse,
            Some(std::cmp::Ordering::Less) => Direction::Better,
            _ => Direction::Same,
        };
        Verdict::Different { p_value: p, direction }
    }
}

/// Render one scenario's comparison section. Format:
///
/// ```text
/// heartbeat-cadence:
///   current  3/50    (6.0%, 95% CI [1.7%, 16.2%])
///   baseline 12/200  (6.0%, 95% CI [3.4%, 10.3%]) at d40e7cf
///   verdict  consistent (p=0.99)
/// ```
pub fn render_comparison(
    scenario: &str,
    failures: u32,
    runs: u32,
    baseline: Option<&Baseline>,
    verdict: Verdict,
) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{scenario}:");
    let cur_ci = wilson_score_95(failures, runs);
    let _ = writeln!(
        out,
        "  current  {failures}/{runs}  ({:.1}%, 95% CI [{:.1}%, {:.1}%])",
        pct(failures, runs),
        cur_ci.lower * 100.0,
        cur_ci.upper * 100.0
    );
    if let Some(b) = baseline {
        let bl_ci = wilson_score_95(b.failures, b.runs);
        let _ = writeln!(
            out,
            "  baseline {}/{}  ({:.1}%, 95% CI [{:.1}%, {:.1}%]) at {}",
            b.failures,
            b.runs,
            pct(b.failures, b.runs),
            bl_ci.lower * 100.0,
            bl_ci.upper * 100.0,
            b.commit
        );
    }
    let _ = writeln!(out, "  verdict  {}", verdict_label(verdict));
    out
}

fn pct(k: u32, n: u32) -> f64 {
    if n == 0 {
        0.0
    } else {
        100.0 * f64::from(k) / f64::from(n)
    }
}

fn verdict_label(v: Verdict) -> String {
    match v {
        Verdict::NoBaseline => "no baseline recorded".to_string(),
        Verdict::Consistent { p_value } => format!("consistent (p={p_value:.2})"),
        Verdict::Different { p_value, direction } => {
            let dir = match direction {
                Direction::Worse => "WORSE",
                Direction::Better => "better",
                Direction::Same => "same rate, different sample size",
            };
            format!("{dir} (p={p_value:.4})")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::baseline::Baseline;
    use time::OffsetDateTime;
    use time::macros::datetime;

    fn baseline(failures: u32, runs: u32) -> Baseline {
        Baseline {
            commit: "abc1234".to_string(),
            build_hash: None,
            runs,
            failures,
            recorded_at: datetime!(2026-06-08 12:00:00 UTC),
        }
    }

    // Avoids unused-import warning when feature isn't on.
    #[allow(dead_code)]
    fn _odt_used() -> OffsetDateTime {
        datetime!(2026-06-08 12:00:00 UTC)
    }

    #[test]
    fn no_baseline_yields_no_baseline_verdict() {
        let v = verdict(3, 50, None, DEFAULT_ALPHA);
        assert_eq!(v, Verdict::NoBaseline);
    }

    #[test]
    fn matching_rates_are_consistent() {
        // 6/100 vs 12/200 — identical proportions.
        let b = baseline(12, 200);
        let v = verdict(6, 100, Some(&b), DEFAULT_ALPHA);
        match v {
            Verdict::Consistent { p_value } => assert!(p_value > 0.95),
            other => panic!("expected Consistent, got {other:?}"),
        }
    }

    #[test]
    fn massively_worse_is_flagged_worse() {
        // 50/100 vs 12/200 — clearly worse.
        let b = baseline(12, 200);
        let v = verdict(50, 100, Some(&b), DEFAULT_ALPHA);
        match v {
            Verdict::Different { p_value, direction } => {
                assert!(p_value < 0.001);
                assert_eq!(direction, Direction::Worse);
            }
            other => panic!("expected Different/Worse, got {other:?}"),
        }
    }

    #[test]
    fn massively_better_is_flagged_better() {
        // 0/200 vs 50/100 — clearly better.
        let b = baseline(50, 100);
        let v = verdict(0, 200, Some(&b), DEFAULT_ALPHA);
        match v {
            Verdict::Different { p_value, direction } => {
                assert!(p_value < 0.001);
                assert_eq!(direction, Direction::Better);
            }
            other => panic!("expected Different/Better, got {other:?}"),
        }
    }

    #[test]
    fn render_with_baseline_contains_expected_fields() {
        let b = baseline(12, 200);
        let v = verdict(3, 50, Some(&b), DEFAULT_ALPHA);
        let out = render_comparison("heartbeat-cadence", 3, 50, Some(&b), v);
        assert!(out.contains("heartbeat-cadence:"));
        assert!(out.contains("current  3/50"));
        assert!(out.contains("baseline 12/200"));
        assert!(out.contains("at abc1234"));
        assert!(out.contains("verdict  "));
    }

    #[test]
    fn render_without_baseline_omits_baseline_line() {
        // "no baseline recorded" in the verdict label is expected;
        // the absent thing is the `  baseline N/M` data line.
        let v = Verdict::NoBaseline;
        let out = render_comparison("heartbeat-cadence", 3, 50, None, v);
        assert!(out.contains("current  3/50"));
        assert!(!out.lines().any(|l| l.starts_with("  baseline ")));
        assert!(out.contains("no baseline recorded"));
    }
}
