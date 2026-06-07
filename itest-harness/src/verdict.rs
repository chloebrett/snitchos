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
use std::time::Duration;

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

/// All the inputs `render_comparison` needs for one scenario. Struct
/// instead of long positional args so adding new dimensions (timing,
/// future telemetry) doesn't churn every call site.
pub struct ComparisonRender<'a> {
    pub scenario: &'a str,
    pub failures: u32,
    pub runs: u32,
    pub mean_duration: Option<Duration>,
    pub p95_duration: Option<Duration>,
    pub baseline: Option<&'a Baseline>,
    pub verdict: Verdict,
}

/// Render one scenario's comparison section. Example with timing:
///
/// ```text
/// heartbeat-cadence:
///   current  3/50    (6.0%, 95% CI [1.7%, 16.2%])
///   baseline 12/200  (6.0%, 95% CI [3.4%, 10.3%]) at d40e7cf
///   timing   1.3s mean (p95 1.4s) vs baseline 1.2s mean (p95 1.3s)
///   verdict  consistent (p=0.99)
/// ```
///
/// The timing line is omitted entirely when neither side has data.
pub fn render_comparison(r: &ComparisonRender<'_>) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{}:", r.scenario);

    let cur_ci = wilson_score_95(r.failures, r.runs);
    let _ = writeln!(
        out,
        "  current  {}/{}  ({:.1}%, 95% CI [{:.1}%, {:.1}%])",
        r.failures,
        r.runs,
        pct(r.failures, r.runs),
        cur_ci.lower * 100.0,
        cur_ci.upper * 100.0
    );

    if let Some(b) = r.baseline {
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

    if let Some(line) = render_timing_line(r) {
        let _ = writeln!(out, "  timing   {line}");
    }

    let _ = writeln!(out, "  verdict  {}", verdict_label(r.verdict));
    out
}

/// Build the `timing` line if we have something to say. Returns
/// `None` when neither side has timing data.
fn render_timing_line(r: &ComparisonRender<'_>) -> Option<String> {
    let baseline_mean_ms = r.baseline.and_then(|b| b.mean_duration_ms);
    let baseline_p95_ms = r.baseline.and_then(|b| b.p95_duration_ms);
    let any = r.mean_duration.is_some()
        || r.p95_duration.is_some()
        || baseline_mean_ms.is_some()
        || baseline_p95_ms.is_some();
    if !any {
        return None;
    }
    let cur_side = match (r.mean_duration, r.p95_duration) {
        (Some(m), Some(p)) => format!("{} mean (p95 {})", fmt_duration(m), fmt_duration(p)),
        (Some(m), None) => format!("{} mean", fmt_duration(m)),
        _ => "no current timing".to_string(),
    };
    let baseline_side = match (baseline_mean_ms, baseline_p95_ms) {
        (Some(m), Some(p)) => format!(
            "baseline {} mean (p95 {})",
            fmt_duration_ms(m),
            fmt_duration_ms(p)
        ),
        (Some(m), None) => format!("baseline {} mean", fmt_duration_ms(m)),
        _ => "no baseline timing".to_string(),
    };
    Some(format!("{cur_side} vs {baseline_side}"))
}

fn fmt_duration(d: Duration) -> String {
    fmt_duration_ms(d.as_secs_f64() * 1000.0)
}

fn fmt_duration_ms(ms: f64) -> String {
    if ms >= 1000.0 {
        format!("{:.1}s", ms / 1000.0)
    } else {
        format!("{:.0}ms", ms)
    }
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
    use time::macros::datetime;

    fn baseline(failures: u32, runs: u32) -> Baseline {
        Baseline {
            commit: "abc1234".to_string(),
            build_hash: None,
            runs,
            failures,
            recorded_at: datetime!(2026-06-08 12:00:00 UTC),
            mean_duration_ms: None,
            p95_duration_ms: None,
        }
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

    fn render(
        failures: u32,
        runs: u32,
        mean: Option<Duration>,
        p95: Option<Duration>,
        baseline: Option<&Baseline>,
        verdict: Verdict,
    ) -> String {
        render_comparison(&ComparisonRender {
            scenario: "heartbeat-cadence",
            failures,
            runs,
            mean_duration: mean,
            p95_duration: p95,
            baseline,
            verdict,
        })
    }

    #[test]
    fn render_with_baseline_contains_expected_fields() {
        let b = baseline(12, 200);
        let v = verdict(3, 50, Some(&b), DEFAULT_ALPHA);
        let out = render(3, 50, None, None, Some(&b), v);
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
        let out = render(3, 50, None, None, None, v);
        assert!(out.contains("current  3/50"));
        assert!(!out.lines().any(|l| l.starts_with("  baseline ")));
        assert!(out.contains("no baseline recorded"));
    }

    #[test]
    fn render_timing_line_appears_when_current_has_data() {
        let v = Verdict::NoBaseline;
        let out = render(
            0,
            5,
            Some(Duration::from_millis(1234)),
            Some(Duration::from_millis(1500)),
            None,
            v,
        );
        // Mean rendered as seconds-with-decimal once ≥ 1s.
        assert!(out.contains("timing   1.2s mean (p95 1.5s)"));
    }

    #[test]
    fn render_timing_line_includes_baseline_side_when_available() {
        let mut b = baseline(12, 200);
        b.mean_duration_ms = Some(1100.0);
        b.p95_duration_ms = Some(1300.0);
        let v = verdict(0, 50, Some(&b), DEFAULT_ALPHA);
        let out = render(
            0,
            50,
            Some(Duration::from_millis(1200)),
            Some(Duration::from_millis(1400)),
            Some(&b),
            v,
        );
        assert!(out.contains("timing   1.2s mean (p95 1.4s)"));
        assert!(out.contains("vs baseline 1.1s mean (p95 1.3s)"));
    }

    #[test]
    fn render_timing_line_absent_when_no_data() {
        let v = Verdict::NoBaseline;
        let out = render(0, 5, None, None, None, v);
        assert!(!out.contains("timing"));
    }

    #[test]
    fn render_short_duration_in_milliseconds() {
        let v = Verdict::NoBaseline;
        let out = render(0, 5, Some(Duration::from_millis(420)), None, None, v);
        assert!(out.contains("timing   420ms mean"));
    }
}
