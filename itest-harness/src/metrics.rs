//! Single source of truth for the baseline metric catalogue.
//!
//! Both exporters — `prom` (textfile) and `otlp` (protobuf push) — emit
//! the *same* per-scenario metrics computed the *same* way; they diverge
//! only in rendering. This module does the per-scenario iteration and
//! value computation **once**, returning a transport-agnostic
//! intermediate representation. Each exporter formats the IR its way.
//! (Before this, adding a metric meant editing both exporters — and
//! forgetting one was a live hazard.)

use crate::baseline::BaselineFile;
use crate::stats::wilson_score_95;

/// A single observed value, carrying whether it's integer- or
/// float-typed (the exporters render the two differently — int vs double
/// data points in OTLP).
pub(crate) enum MetricValue {
    Int(i64),
    Float(f64),
}

/// One data point: its label set (always `scenario`, plus `signature`
/// for the per-bucket series) and value.
pub(crate) struct MetricPoint {
    pub labels: Vec<(&'static str, String)>,
    pub value: MetricValue,
}

/// One metric across all scenarios: a name suffix (the exporters prefix
/// `snitchos_itest_baseline_` / `snitchos.itest.baseline.`), help text,
/// OTLP unit, and the points.
pub(crate) struct MetricSeries {
    pub suffix: &'static str,
    pub help: &'static str,
    pub unit: &'static str,
    pub points: Vec<MetricPoint>,
}

/// Compute the full baseline metric catalogue from `file`, iterating
/// each scenario's `current` once. Series with no points (e.g. duration
/// metrics when nothing recorded timing, or `signature` with no
/// failures) are returned empty — `prom` still emits their HELP/TYPE,
/// `otlp` skips them.
pub(crate) fn baseline_metrics(file: &BaselineFile) -> Vec<MetricSeries> {
    let mut runs = series("runs", "Number of --repeat iterations in the current baseline.", "1");
    let mut failures = series("failures", "Number of failed iterations in the current baseline.", "1");
    let mut rate = series("failure_rate", "Observed failure rate in the current baseline (0.0-1.0).", "1");
    let mut wlo = series("wilson_lower", "Wilson-score 95% CI lower bound on the failure rate.", "1");
    let mut wup = series("wilson_upper", "Wilson-score 95% CI upper bound on the failure rate.", "1");
    let mut mean = series("mean_duration_ms", "Mean per-iteration wall-clock duration, milliseconds.", "ms");
    let mut p95 = series("p95_duration_ms", "p95 per-iteration wall-clock duration, milliseconds.", "ms");
    let mut partial = series("partial", "1 if the current baseline reflects an interrupted run, else 0.", "1");
    let mut recorded = series("recorded_at_seconds", "Unix timestamp (seconds) when the current baseline was recorded.", "s");
    let mut signature = series("signature", "Per-scenario failure count by cause-bucket.", "1");

    for (name, entry) in &file.scenarios {
        let Some(b) = &entry.current else { continue };
        let ci = wilson_score_95(b.failures, b.runs);
        let r = if b.runs == 0 {
            0.0
        } else {
            f64::from(b.failures) / f64::from(b.runs)
        };
        let label = || vec![("scenario", name.clone())];

        runs.points.push(point(label(), MetricValue::Int(i64::from(b.runs))));
        failures.points.push(point(label(), MetricValue::Int(i64::from(b.failures))));
        rate.points.push(point(label(), MetricValue::Float(r)));
        wlo.points.push(point(label(), MetricValue::Float(ci.lower)));
        wup.points.push(point(label(), MetricValue::Float(ci.upper)));
        if let Some(m) = b.mean_duration_ms {
            mean.points.push(point(label(), MetricValue::Float(m)));
        }
        if let Some(p) = b.p95_duration_ms {
            p95.points.push(point(label(), MetricValue::Float(p)));
        }
        partial.points.push(point(label(), MetricValue::Int(i64::from(b.partial.is_some()))));
        recorded.points.push(point(label(), MetricValue::Int(b.recorded_at.unix_timestamp())));
        for (sig, count) in &b.signature_counts {
            let mut labels = label();
            labels.push(("signature", sig.label().to_string()));
            signature.points.push(point(labels, MetricValue::Int(i64::from(*count))));
        }
    }

    vec![runs, failures, rate, wlo, wup, mean, p95, partial, recorded, signature]
}

fn series(suffix: &'static str, help: &'static str, unit: &'static str) -> MetricSeries {
    MetricSeries { suffix, help, unit, points: Vec::new() }
}

fn point(labels: Vec<(&'static str, String)>, value: MetricValue) -> MetricPoint {
    MetricPoint { labels, value }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::baseline::{Baseline, BaselineFile};
    use crate::signature::Signature;
    use std::collections::BTreeMap;
    use time::macros::datetime;

    fn find<'a>(series: &'a [MetricSeries], suffix: &str) -> &'a MetricSeries {
        series.iter().find(|s| s.suffix == suffix).expect("series present")
    }

    #[test]
    fn baseline_metrics_computes_each_series_once_per_scenario() {
        let mut f = BaselineFile::new();
        f.update_current(
            "scn",
            Baseline {
                commit: "abc1234".to_string(),
                build_hash: None,
                runs: 100,
                failures: 3,
                recorded_at: datetime!(2026-06-08 12:00:00 UTC),
                mean_duration_ms: Some(1200.0),
                p95_duration_ms: None,
                partial: None,
                signature_counts: BTreeMap::from([(Signature::Wedge, 2u32)]),
            },
        );
        let s = baseline_metrics(&f);

        // runs: one Int point of 100.
        assert_eq!(find(&s, "runs").points.len(), 1);
        assert!(matches!(find(&s, "runs").points[0].value, MetricValue::Int(100)));
        // failure_rate: Float 0.03.
        assert!(matches!(find(&s, "failure_rate").points[0].value, MetricValue::Float(v) if (v - 0.03).abs() < 1e-9));
        // p95 unset → empty (but the series still exists, for HELP/TYPE).
        assert!(find(&s, "p95_duration_ms").points.is_empty());
        // signature: one point carrying scenario + signature labels, Int(2).
        let sig = &find(&s, "signature").points[0];
        assert!(sig.labels.iter().any(|(k, v)| *k == "scenario" && v == "scn"));
        assert!(sig.labels.iter().any(|(k, v)| *k == "signature" && v == "wedge"));
        assert!(matches!(sig.value, MetricValue::Int(2)));
    }
}
