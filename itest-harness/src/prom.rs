//! Prometheus textfile-collector export for the baseline file.
//!
//! Emits one `.prom` file's worth of gauges describing the current
//! per-scenario state: runs, failures, rate, Wilson CI, timing,
//! partial flag, and recorded-at timestamp. Designed to be scraped by
//! `node_exporter --collector.textfile.directory=<dir>` so flake-rate
//! and timing trends land in Grafana alongside the rest of the
//! `SnitchOS` observability story.
//!
//! Atomic-rename pattern: writers should write to `<path>.$$` then
//! `rename` into `<path>` so partially-written files never get
//! scraped. `write_atomic` does this.
//!
//! See `plans/itest-history-and-pending.md` step H1.

use std::fmt::Write as _;
use std::io;
use std::path::Path;

use crate::baseline::BaselineFile;
use crate::metrics::{MetricValue, baseline_metrics};

/// Render `file` as a Prometheus textfile-format string. One block of
/// gauges per scenario whose `current` baseline is set. Scenarios with
/// no `current` are skipped — emitting `NaN` would clutter Grafana
/// without adding signal.
pub fn render_prometheus(file: &BaselineFile) -> String {
    let mut out = String::new();
    // One block per metric series: HELP/TYPE then its samples. The metric
    // catalogue + per-scenario values come from `metrics::baseline_metrics`
    // (shared with the OTLP exporter); this function only renders.
    for s in baseline_metrics(file) {
        let name = format!("snitchos_itest_baseline_{}", s.suffix);
        let _ = writeln!(out, "# HELP {name} {}", s.help);
        let _ = writeln!(out, "# TYPE {name} gauge");
        for p in &s.points {
            let labels = p
                .labels
                .iter()
                .map(|(k, v)| format!("{k}=\"{}\"", escape_label_value(v)))
                .collect::<Vec<_>>()
                .join(",");
            match p.value {
                MetricValue::Int(i) => {
                    let _ = writeln!(out, "{name}{{{labels}}} {i}");
                }
                MetricValue::Float(f) => {
                    let _ = writeln!(out, "{name}{{{labels}}} {f}");
                }
            }
        }
    }
    out
}

/// Write `contents` to `path` atomically via the standard
/// "write-to-temp, rename" dance. The textfile collector reads
/// `.prom` files asynchronously; this prevents it from scraping a
/// half-written file.
///
/// Temp suffix uses the current process ID so concurrent writers from
/// different processes don't collide. (Within one process,
/// concurrent calls to `write_atomic` for the same `path` would race
/// — that's a programming error, not something we guard against
/// here.)
pub fn write_atomic(path: &Path, contents: &str) -> io::Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!(
        "{}.{}.tmp",
        path.extension().and_then(|s| s.to_str()).unwrap_or(""),
        std::process::id()
    ));
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Escape a Prometheus label-value per the exposition spec: backslash,
/// double-quote, and newline must be escaped. Scenario names are
/// caller-controlled (kebab-case in practice), but this keeps the
/// renderer robust to future names with odd characters.
fn escape_label_value(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::baseline::PartialMarker;
    use crate::test_support::baseline_with;
    use time::macros::datetime;

    #[test]
    fn render_emits_help_and_type_lines_once_per_metric() {
        let mut f = BaselineFile::new();
        f.update_current("x", baseline_with(100, 5));
        f.update_current("y", baseline_with(50, 0));
        let out = render_prometheus(&f);
        // Each metric should have exactly one HELP and one TYPE.
        assert_eq!(out.matches("# HELP snitchos_itest_baseline_runs ").count(), 1);
        assert_eq!(out.matches("# TYPE snitchos_itest_baseline_runs gauge").count(), 1);
        assert_eq!(out.matches("# HELP snitchos_itest_baseline_failure_rate ").count(), 1);
    }

    #[test]
    fn render_emits_per_scenario_per_signature_gauges() {
        use crate::signature::Signature;
        let mut f = BaselineFile::new();
        let mut b = baseline_with(50, 5);
        b.signature_counts.insert(Signature::Wedge, 2);
        b.signature_counts.insert(Signature::BudgetExhausted, 3);
        f.update_current("workload-cooperative-baseline", b);
        let out = render_prometheus(&f);
        assert!(out.contains("# TYPE snitchos_itest_baseline_signature gauge"));
        assert!(out.contains(
            "snitchos_itest_baseline_signature{scenario=\"workload-cooperative-baseline\",signature=\"wedge\"} 2"
        ));
        assert!(out.contains(
            "snitchos_itest_baseline_signature{scenario=\"workload-cooperative-baseline\",signature=\"budget_exhausted\"} 3"
        ));
    }

    #[test]
    fn render_emits_per_scenario_gauges() {
        let mut f = BaselineFile::new();
        f.update_current("heartbeat-cadence", baseline_with(100, 3));
        let out = render_prometheus(&f);
        assert!(out.contains(
            "snitchos_itest_baseline_runs{scenario=\"heartbeat-cadence\"} 100"
        ));
        assert!(out.contains(
            "snitchos_itest_baseline_failures{scenario=\"heartbeat-cadence\"} 3"
        ));
        assert!(out.contains(
            "snitchos_itest_baseline_failure_rate{scenario=\"heartbeat-cadence\"} 0.03"
        ));
        assert!(out.contains(
            "snitchos_itest_baseline_mean_duration_ms{scenario=\"heartbeat-cadence\"} 1200"
        ));
        assert!(out.contains(
            "snitchos_itest_baseline_partial{scenario=\"heartbeat-cadence\"} 0"
        ));
    }

    #[test]
    fn render_marks_partial_baselines() {
        let mut f = BaselineFile::new();
        let mut b = baseline_with(27, 1);
        b.partial = Some(PartialMarker {
            requested_runs: 100,
            interrupted_at: datetime!(2026-06-08 12:30:00 UTC),
            run_dir: None,
        });
        f.update_current("scn", b);
        let out = render_prometheus(&f);
        assert!(out.contains("snitchos_itest_baseline_partial{scenario=\"scn\"} 1"));
    }

    #[test]
    fn render_skips_scenarios_without_current() {
        let mut f = BaselineFile::new();
        f.update_current("real", baseline_with(10, 0));
        f.scenarios.insert("ghost".to_string(), Default::default());
        let out = render_prometheus(&f);
        assert!(out.contains("scenario=\"real\""));
        assert!(!out.contains("scenario=\"ghost\""));
    }

    #[test]
    fn render_omits_timing_lines_when_absent() {
        let mut f = BaselineFile::new();
        let mut b = baseline_with(10, 0);
        b.mean_duration_ms = None;
        b.p95_duration_ms = None;
        f.update_current("scn", b);
        let out = render_prometheus(&f);
        assert!(!out.contains("snitchos_itest_baseline_mean_duration_ms{scenario=\"scn\""));
        assert!(!out.contains("snitchos_itest_baseline_p95_duration_ms{scenario=\"scn\""));
    }

    #[test]
    fn escape_label_value_escapes_quotes_and_backslashes() {
        assert_eq!(escape_label_value(r#"a"b\c"#), r#"a\"b\\c"#);
        assert_eq!(escape_label_value("line1\nline2"), "line1\\nline2");
        assert_eq!(escape_label_value("plain-name"), "plain-name");
    }

    #[test]
    fn write_atomic_overwrites_destination() {
        let dir = std::env::temp_dir().join(format!(
            "itest-harness-prom-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("baseline.prom");
        write_atomic(&path, "first\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "first\n");
        write_atomic(&path, "second\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "second\n");
        // No leftover tmp files.
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(std::result::Result::ok)
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "tmp files left behind: {leftovers:?}");
        std::fs::remove_dir_all(&dir).ok();
    }
}
