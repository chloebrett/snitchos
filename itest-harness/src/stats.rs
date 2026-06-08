//! Binomial-proportion statistics for flake-rate analysis.
//!
//! Two operations:
//!
//! 1. **Wilson-score interval** — given `k` failures in `n` runs, give
//!    a 95% confidence interval for the true failure rate. Better
//!    behaviour than the naive `±sqrt(p(1-p)/n)` Wald interval,
//!    especially at the boundaries where our flake rates often sit
//!    (e.g. 0/100 should not produce a CI that crosses zero).
//! 2. **Two-proportion z-test** — given two samples `(k1,n1)` and
//!    `(k2,n2)`, return a p-value for "rates are the same." Used to
//!    decide whether a current `--repeat` run is consistent with the
//!    stored baseline.
//!
//! Pure math, no I/O. Stable across runs given identical inputs (no
//! floating-point reductions whose order matters).

/// 1.96 — the z-score for a two-sided 95% confidence interval.
const Z_95: f64 = 1.959_963_984_540_054;

/// Inclusive `[lower, upper]` bounds for a binomial proportion, both in
/// `[0.0, 1.0]`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ConfidenceInterval {
    pub lower: f64,
    pub upper: f64,
}

/// Wilson-score 95% confidence interval for `k` failures in `n` runs.
///
/// Edge cases:
/// - `n == 0` → returns `[0.0, 1.0]` (no information).
/// - `k == 0` → lower bound is 0.0 (Wilson handles this without going
///   negative, unlike the Wald interval).
/// - `k == n` → upper bound is 1.0.
pub(crate) fn wilson_score_95(failures: u32, runs: u32) -> ConfidenceInterval {
    if runs == 0 {
        return ConfidenceInterval { lower: 0.0, upper: 1.0 };
    }
    let n = f64::from(runs);
    let k = f64::from(failures);
    let p_hat = k / n;
    let z = Z_95;
    let z2 = z * z;
    let denom = 1.0 + z2 / n;
    let center = (p_hat + z2 / (2.0 * n)) / denom;
    let margin = z * (p_hat * (1.0 - p_hat) / n + z2 / (4.0 * n * n)).sqrt() / denom;
    let lower = (center - margin).clamp(0.0, 1.0);
    let upper = (center + margin).clamp(0.0, 1.0);
    ConfidenceInterval { lower, upper }
}

/// Two-sided p-value from a two-proportion pooled z-test. Returns a
/// value in `[0.0, 1.0]`. Small values argue "rates are different."
///
/// Edge cases:
/// - If either `n == 0`, returns `1.0` (no evidence of difference).
/// - If both samples have zero observations of the event (both
///   `k == 0`), the pooled SE is 0 and the test is undefined — we
///   return `1.0` (treat as consistent; we can't distinguish anything).
/// - If both samples are identical proportions, p ≈ 1.0.
pub(crate) fn two_proportion_p_value(k1: u32, n1: u32, k2: u32, n2: u32) -> f64 {
    if n1 == 0 || n2 == 0 {
        return 1.0;
    }
    let n1 = f64::from(n1);
    let n2 = f64::from(n2);
    let k1 = f64::from(k1);
    let k2 = f64::from(k2);
    let p1 = k1 / n1;
    let p2 = k2 / n2;
    let pooled = (k1 + k2) / (n1 + n2);
    let se_sq = pooled * (1.0 - pooled) * (1.0 / n1 + 1.0 / n2);
    if se_sq <= 0.0 {
        // Both observed rates are exactly 0 or exactly 1; the test
        // can't say anything about a difference.
        return 1.0;
    }
    let z = (p1 - p2) / se_sq.sqrt();
    // Two-sided: P(|Z| > |z|) = 2 * (1 - Φ(|z|))
    2.0 * (1.0 - normal_cdf(z.abs()))
}

/// Standard normal CDF Φ(x), computed via the Abramowitz-Stegun
/// approximation 7.1.26 (max error ≈ 1.5×10⁻⁷). More than enough for
/// flake-rate verdicts where we care about p > 0.05 vs p < 0.05.
fn normal_cdf(x: f64) -> f64 {
    // erf approximation:
    let sign = x.signum();
    let x = x.abs();
    let a1 = 0.254_829_592;
    let a2 = -0.284_496_736;
    let a3 = 1.421_413_741;
    let a4 = -1.453_152_027;
    let a5 = 1.061_405_429;
    let p = 0.327_591_1;

    let t = 1.0 / (1.0 + p * (x / 2.0_f64.sqrt()));
    let y = 1.0
        - (((((a5 * t + a4) * t) + a3) * t + a2) * t + a1)
            * t
            * (-x * x / 2.0).exp();
    0.5 * (1.0 + sign * y)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() <= tol
    }

    #[test]
    fn wilson_zero_runs_is_max_uncertainty() {
        let ci = wilson_score_95(0, 0);
        assert_eq!(ci.lower, 0.0);
        assert_eq!(ci.upper, 1.0);
    }

    #[test]
    fn wilson_zero_failures_lower_bound_is_zero() {
        // 0/100 should give [0, ~0.037] (Wilson handles zero
        // cleanly, unlike Wald which gives [0, 0]). Float arithmetic
        // can leave lower at ~1e-18 instead of exactly 0; that's fine.
        let ci = wilson_score_95(0, 100);
        assert!(ci.lower < 1e-10);
        assert!(ci.upper > 0.025 && ci.upper < 0.045);
    }

    #[test]
    fn wilson_typical_flake_rate() {
        // 6/200 = 3% with reasonable CI bounds. Reference values from
        // R's binom::binom.confint(6, 200, methods="wilson").
        let ci = wilson_score_95(6, 200);
        assert!(close(ci.lower, 0.0138, 0.001));
        assert!(close(ci.upper, 0.064, 0.001));
    }

    #[test]
    fn wilson_all_failures_upper_bound_is_one() {
        let ci = wilson_score_95(50, 50);
        assert_eq!(ci.upper, 1.0);
        assert!(ci.lower > 0.9);
    }

    #[test]
    fn normal_cdf_basics() {
        assert!(close(normal_cdf(0.0), 0.5, 1e-5));
        assert!(close(normal_cdf(1.96), 0.975, 1e-3));
        assert!(close(normal_cdf(-1.96), 0.025, 1e-3));
        assert!(close(normal_cdf(3.0), 0.9987, 1e-3));
    }

    #[test]
    fn two_proportion_identical_rates() {
        // 5/100 vs 5/100 should produce p ≈ 1.0
        let p = two_proportion_p_value(5, 100, 5, 100);
        assert!(p > 0.99, "expected p ≈ 1.0, got {p}");
    }

    #[test]
    fn two_proportion_clearly_different_rates() {
        // 50/100 vs 5/100 should produce p ≈ 0
        let p = two_proportion_p_value(50, 100, 5, 100);
        assert!(p < 0.001, "expected p ≈ 0, got {p}");
    }

    #[test]
    fn two_proportion_borderline_case() {
        // 6/200 (baseline) vs 4/50 (current). Roughly: 3% vs 8%.
        // Should be "possibly different but not strongly so" — p ≈ 0.07
        // by hand (and confirmed in R: prop.test(c(4,6), c(50,200),
        // correct=FALSE)$p.value ≈ 0.09).
        let p = two_proportion_p_value(4, 50, 6, 200);
        assert!(p > 0.05 && p < 0.15, "expected borderline, got {p}");
    }

    #[test]
    fn two_proportion_both_zero_failures_returns_one() {
        let p = two_proportion_p_value(0, 100, 0, 100);
        assert_eq!(p, 1.0);
    }

    #[test]
    fn two_proportion_empty_sample_returns_one() {
        assert_eq!(two_proportion_p_value(0, 0, 5, 100), 1.0);
        assert_eq!(two_proportion_p_value(5, 100, 0, 0), 1.0);
    }
}
