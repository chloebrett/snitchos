//! Prometheus textfile-collector export for the baseline file.
//!
//! Emits one `.prom` file's worth of gauges describing the current
//! per-scenario state: runs, failures, rate, Wilson CI, timing,
//! partial flag, and recorded-at timestamp. Designed to be scraped by
//! `node_exporter --collector.textfile.directory=<dir>` so flake-rate
//! and timing trends land in Grafana alongside the rest of the
//! SnitchOS observability story.
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
use crate::stats::wilson_score_95;

/// Render `file` as a Prometheus textfile-format string. One block of
/// gauges per scenario whose `current` baseline is set. Scenarios with
/// no `current` are skipped — emitting `NaN` would clutter Grafana
/// without adding signal.
pub fn render_prometheus(file: &BaselineFile) -> String {
    let mut out = String::new();

    // HELP/TYPE preambles — emitted once each, not per scenario.
    let metrics = [
        ("snitchos_itest_baseline_runs", "Number of --repeat iterations in the current baseline."),
        ("snitchos_itest_baseline_failures", "Number of failed iterations in the current baseline."),
        ("snitchos_itest_baseline_failure_rate", "Observed failure rate in the current baseline (0.0–1.0)."),
        ("snitchos_itest_baseline_wilson_lower", "Wilson-score 95% CI lower bound on the failure rate."),
        ("snitchos_itest_baseline_wilson_upper", "Wilson-score 95% CI upper bound on the failure rate."),
        ("snitchos_itest_baseline_mean_duration_ms", "Mean per-iteration wall-clock duration, milliseconds."),
        ("snitchos_itest_baseline_p95_duration_ms", "p95 per-iteration wall-clock duration, milliseconds."),
        ("snitchos_itest_baseline_partial", "1 if the current baseline reflects an interrupted run, else 0."),
        ("snitchos_itest_baseline_recorded_at_seconds", "Unix timestamp (seconds) when the current baseline was recorded."),
    ];
    for (name, help) in metrics {
        let _ = writeln!(out, "# HELP {name} {help}");
        let _ = writeln!(out, "# TYPE {name} gauge");
    }

    for (name, entry) in &file.scenarios {
        let Some(b) = &entry.current else { continue };
        let label = format!("scenario=\"{}\"", escape_label_value(name));
        let ci = wilson_score_95(b.failures, b.runs);
        let rate = if b.runs == 0 {
            0.0
        } else {
            f64::from(b.failures) / f64::from(b.runs)
        };
        let partial = if b.partial.is_some() { 1 } else { 0 };
        let recorded_at_secs = b.recorded_at.unix_timestamp();

        let _ = writeln!(out, "snitchos_itest_baseline_runs{{{label}}} {}", b.runs);
        let _ = writeln!(out, "snitchos_itest_baseline_failures{{{label}}} {}", b.failures);
        let _ = writeln!(out, "snitchos_itest_baseline_failure_rate{{{label}}} {rate}");
        let _ = writeln!(out, "snitchos_itest_baseline_wilson_lower{{{label}}} {}", ci.lower);
        let _ = writeln!(out, "snitchos_itest_baseline_wilson_upper{{{label}}} {}", ci.upper);
        if let Some(m) = b.mean_duration_ms {
            let _ = writeln!(out, "snitchos_itest_baseline_mean_duration_ms{{{label}}} {m}");
        }
        if let Some(p) = b.p95_duration_ms {
            let _ = writeln!(out, "snitchos_itest_baseline_p95_duration_ms{{{label}}} {p}");
        }
        let _ = writeln!(out, "snitchos_itest_baseline_partial{{{label}}} {partial}");
        let _ = writeln!(out, "snitchos_itest_baseline_recorded_at_seconds{{{label}}} {recorded_at_secs}");
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
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
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
    use crate::baseline::{Baseline, PartialMarker};
    use time::macros::datetime;

    fn baseline_with(runs: u32, failures: u32) -> Baseline {
        Baseline {
            commit: "abc1234".to_string(),
            build_hash: None,
            runs,
            failures,
            recorded_at: datetime!(2026-06-08 12:00:00 UTC),
            mean_duration_ms: Some(1200.0),
            p95_duration_ms: Some(1500.0),
            partial: None,
        }
    }

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
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "tmp files left behind: {leftovers:?}");
        std::fs::remove_dir_all(&dir).ok();
    }
}
