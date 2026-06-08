//! Versioned per-scenario flake-rate baseline, stored as TOML in the
//! repo.
//!
//! Schema:
//!
//! ```toml
//! [scenarios.heartbeat-cadence.current]
//! commit = "d40e7cf"
//! build_hash = "8d3f..."
//! runs = 200
//! failures = 12
//! recorded_at = "2026-06-08T15:42:33Z"
//!
//! [[scenarios.heartbeat-cadence.history]]
//! commit = "efcbbf9"
//! runs = 100
//! failures = 8
//! recorded_at = "2026-06-07T09:30:00Z"
//! ```
//!
//! Timestamps are RFC 3339 strings rather than TOML's native datetime
//! type — the `time` crate's serde adapter parses strings, not the
//! `toml::value::Datetime` map shape. Visually one quote-pair away
//! from native; functionally simpler.
//!
//! `current` is what live runs compare against. `history` is
//! append-only — `update_current` pushes the previous `current` into
//! `history` before replacing it.
//!
//! No I/O is mandatory in this module — `load_str` / `to_string` work
//! against in-memory strings so tests don't touch the filesystem.
//! `load_path` / `save_path` are convenience wrappers.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::serde::rfc3339;

/// A single baseline measurement for one scenario.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Baseline {
    /// Git commit at which this measurement was taken.
    pub commit: String,
    /// SHA of the kernel ELF at measurement time. Optional — older
    /// records may not have it. Used as a sanity check that the
    /// current build matches what was measured; mismatch is a warning,
    /// not an error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_hash: Option<String>,
    /// Number of `--repeat` iterations.
    pub runs: u32,
    /// Number of those runs in which this scenario failed.
    pub failures: u32,
    /// When the measurement was taken. RFC 3339 UTC.
    #[serde(with = "rfc3339")]
    pub recorded_at: OffsetDateTime,
    /// Mean per-iteration wall-clock time in milliseconds. Optional
    /// for back-compat: older baseline files don't have this and
    /// callers shouldn't crash on absence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mean_duration_ms: Option<f64>,
    /// p95 per-iteration wall-clock time in milliseconds. Same
    /// back-compat treatment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p95_duration_ms: Option<f64>,
    /// Present only when this baseline reflects an *interrupted* run
    /// (Ctrl-C / SIGINT before all `--repeat` iterations completed).
    /// The summary stats above are computed from however many runs
    /// did finish. Promotion still works — `--promote-pending`
    /// strips this field — but `--baseline-show` surfaces the
    /// partial-ness to the user.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partial: Option<PartialMarker>,
}

/// Metadata for a partial baseline: how short of the request we
/// stopped, when, and the per-run history directory the data came
/// from (so it can be reconstructed if the pending file is lost).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PartialMarker {
    /// What `--repeat N` was requested. `runs` on the surrounding
    /// `Baseline` is what actually completed.
    pub requested_runs: u32,
    /// Wall-clock UTC when the interrupt fired.
    #[serde(with = "rfc3339")]
    pub interrupted_at: OffsetDateTime,
    /// Relative path to the per-run history directory under
    /// `.itest-runs/`. Used by `--recover-pending` to rebuild this
    /// entry from the underlying NDJSON if the pending file is
    /// lost or corrupted. `None` when no history directory exists —
    /// step B writes pending files without one; step C onward fills
    /// it in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_dir: Option<String>,
}

impl Baseline {
    pub fn rate(&self) -> f64 {
        if self.runs == 0 {
            0.0
        } else {
            f64::from(self.failures) / f64::from(self.runs)
        }
    }
}

/// Per-scenario `current` pointer plus its history. `current` may be
/// absent (a scenario the file knows about but has never recorded a
/// baseline for) — useful when committing a fresh scenario row that
/// will be populated by the next `--update-baseline` run.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScenarioBaseline {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<Baseline>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<Baseline>,
}

/// Root document of the baseline TOML.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct BaselineFile {
    #[serde(default)]
    pub scenarios: BTreeMap<String, ScenarioBaseline>,
}

impl BaselineFile {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace `current` for the given scenario with `new`, pushing the
    /// previous `current` (if any) onto `history`. History is kept in
    /// chronological order of insertion.
    pub fn update_current(&mut self, scenario: &str, new: Baseline) {
        let entry = self
            .scenarios
            .entry(scenario.to_string())
            .or_default();
        if let Some(previous) = entry.current.take() {
            entry.history.push(previous);
        }
        entry.current = Some(new);
    }

    pub fn current_for(&self, scenario: &str) -> Option<&Baseline> {
        self.scenarios.get(scenario).and_then(|s| s.current.as_ref())
    }

    /// Parse the TOML representation. Returns the parse error verbatim
    /// so the caller can surface it to the user.
    pub fn load_str(s: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(s)
    }

    /// Render to TOML. The output is suitable for committing to the
    /// repo; insertion order is determined by the `BTreeMap`, so the
    /// serialised form is stable across re-saves.
    pub fn to_string(&self) -> Result<String, toml::ser::Error> {
        toml::to_string_pretty(self)
    }

    pub fn load_path(path: &Path) -> Result<Self, BaselineError> {
        let s = fs::read_to_string(path).map_err(BaselineError::Io)?;
        Self::load_str(&s).map_err(BaselineError::Toml)
    }

    pub fn save_path(&self, path: &Path) -> Result<(), BaselineError> {
        let s = self.to_string().map_err(BaselineError::TomlSer)?;
        fs::write(path, s).map_err(BaselineError::Io)
    }

    /// Conventional path of the pending sidecar for `canonical_path`.
    /// Appends `.pending` to the filename — e.g.
    /// `.itest-baseline.toml` → `.itest-baseline.toml.pending`.
    pub fn pending_path_for(canonical_path: &Path) -> PathBuf {
        let mut p = canonical_path.to_path_buf();
        let mut name = p
            .file_name()
            .unwrap_or_default()
            .to_os_string();
        name.push(".pending");
        p.set_file_name(name);
        p
    }

    /// Promote the pending file at `<canonical>.pending` into
    /// `canonical_path`: each scenario's `current` in the pending
    /// becomes the new canonical `current` (the previous one is
    /// pushed to `history` per `update_current`). The `partial`
    /// marker is stripped on promotion. Pending file is removed
    /// after a successful write. Returns the updated canonical.
    pub fn promote_pending(canonical_path: &Path) -> Result<Self, BaselineError> {
        let pending_path = Self::pending_path_for(canonical_path);
        if !pending_path.exists() {
            return Err(BaselineError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("no pending file at {}", pending_path.display()),
            )));
        }
        let pending = Self::load_path(&pending_path)?;
        let mut canonical = if canonical_path.exists() {
            Self::load_path(canonical_path)?
        } else {
            Self::default()
        };
        for (name, entry) in pending.scenarios {
            if let Some(mut current) = entry.current {
                current.partial = None; // promoted entries are no longer partial
                canonical.update_current(&name, current);
            }
        }
        canonical.save_path(canonical_path)?;
        std::fs::remove_file(&pending_path).map_err(BaselineError::Io)?;
        Ok(canonical)
    }

    /// Delete the pending sidecar at `<canonical>.pending` if it
    /// exists. Idempotent: no-op when the file is already absent.
    pub fn discard_pending(canonical_path: &Path) -> std::io::Result<()> {
        let pending_path = Self::pending_path_for(canonical_path);
        if pending_path.exists() {
            std::fs::remove_file(&pending_path)?;
        }
        Ok(())
    }

    /// Render the file as a human-readable summary for `--baseline-show`.
    /// See `SummaryOptions` for the filter/sort knobs.
    pub fn render_summary(&self, opts: SummaryOptions) -> String {
        use std::fmt::Write;
        let mut out = String::new();
        if self.scenarios.is_empty() {
            let _ = writeln!(out, "(no scenarios recorded)");
            return out;
        }

        // Pre-collect so we can filter and sort. Bias is toward
        // surfacing "most-confidently-flaky" first when sorting; the
        // alphabetical default is fine when not.
        let mut entries: Vec<(&String, &ScenarioBaseline)> = self
            .scenarios
            .iter()
            .filter(|(_, e)| {
                if !opts.flakes_only {
                    return true;
                }
                e.current.as_ref().is_some_and(|b| b.failures > 0)
            })
            .collect();

        if opts.flakes_only {
            // Sort descending by Wilson lower bound, then upper bound.
            // The lower bound is "how confident are we this scenario
            // is at-least-this-flaky" — a high lower bound is the
            // most worrying signal. Upper bound tie-breaks ties.
            entries.sort_by(|(_, a), (_, b)| {
                let ci_a = a.current.as_ref().map(|b| crate::stats::wilson_score_95(b.failures, b.runs));
                let ci_b = b.current.as_ref().map(|b| crate::stats::wilson_score_95(b.failures, b.runs));
                let key_a = ci_a.map(|c| (c.lower, c.upper)).unwrap_or((0.0, 0.0));
                let key_b = ci_b.map(|c| (c.lower, c.upper)).unwrap_or((0.0, 0.0));
                // Descending: b before a.
                key_b
                    .partial_cmp(&key_a)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        }

        if entries.is_empty() {
            let _ = writeln!(out, "(no flaky scenarios recorded)");
            return out;
        }

        for (name, entry) in entries {
            let _ = writeln!(out, "{name}");
            if let Some(b) = &entry.current {
                let _ = writeln!(out, "  current  {}", render_baseline(b));
            } else {
                let _ = writeln!(out, "  current  (none)");
            }
            if opts.include_history && !entry.history.is_empty() {
                let _ = writeln!(out, "  history:");
                for b in &entry.history {
                    let _ = writeln!(out, "    {}", render_baseline(b));
                }
            }
        }
        out
    }
}

/// Filter and sort knobs for `BaselineFile::render_summary`. All
/// default to `false` (full alphabetical listing of `current` only).
#[derive(Debug, Clone, Copy, Default)]
pub struct SummaryOptions {
    /// Include each scenario's prior `current` entries (now in
    /// `history`) below its current measurement.
    pub include_history: bool,
    /// Filter out scenarios whose `current` measurement has zero
    /// failures, and sort the remainder descending by Wilson-score
    /// lower bound (tie-break: upper bound). Surfaces "the most
    /// confidently flaky scenario" at the top.
    pub flakes_only: bool,
}

fn render_baseline(b: &Baseline) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let pct = if b.runs == 0 {
        0.0
    } else {
        100.0 * f64::from(b.failures) / f64::from(b.runs)
    };
    let ci = crate::stats::wilson_score_95(b.failures, b.runs);
    let _ = write!(
        out,
        "{}/{}  ({:.1}%, 95% CI [{:.1}%, {:.1}%]) at {}, recorded {}",
        b.failures,
        b.runs,
        pct,
        ci.lower * 100.0,
        ci.upper * 100.0,
        b.commit,
        b.recorded_at
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_else(|_| "(invalid timestamp)".to_string())
    );
    if let Some(hash) = &b.build_hash {
        let _ = write!(out, " build={hash}");
    }
    if let (Some(mean), Some(p95)) = (b.mean_duration_ms, b.p95_duration_ms) {
        let _ = write!(out, "\n             timing: mean {:.0}ms, p95 {:.0}ms", mean, p95);
    } else if let Some(mean) = b.mean_duration_ms {
        let _ = write!(out, "\n             timing: mean {:.0}ms", mean);
    }
    out
}

#[derive(Debug)]
pub enum BaselineError {
    Io(std::io::Error),
    Toml(toml::de::Error),
    TomlSer(toml::ser::Error),
}

impl std::fmt::Display for BaselineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BaselineError::Io(e) => write!(f, "io error: {e}"),
            BaselineError::Toml(e) => write!(f, "toml parse error: {e}"),
            BaselineError::TomlSer(e) => write!(f, "toml serialize error: {e}"),
        }
    }
}

impl std::error::Error for BaselineError {}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    fn make_baseline(commit: &str, runs: u32, failures: u32, at: OffsetDateTime) -> Baseline {
        Baseline {
            commit: commit.to_string(),
            build_hash: None,
            runs,
            failures,
            recorded_at: at,
            mean_duration_ms: None,
            p95_duration_ms: None,
            partial: None,
        }
    }

    #[test]
    fn empty_file_round_trips() {
        let f = BaselineFile::new();
        let s = f.to_string().unwrap();
        let parsed = BaselineFile::load_str(&s).unwrap();
        assert_eq!(parsed, f);
    }

    #[test]
    fn baseline_with_one_current_round_trips() {
        let mut f = BaselineFile::new();
        f.update_current(
            "heartbeat-cadence",
            make_baseline("d40e7cf", 200, 12, datetime!(2026-06-08 15:42:33 UTC)),
        );
        let s = f.to_string().unwrap();
        // Sanity-check the TOML shape so a schema change announces itself.
        assert!(s.contains("[scenarios.heartbeat-cadence.current]"));
        assert!(s.contains("commit = \"d40e7cf\""));
        assert!(s.contains("runs = 200"));
        let parsed = BaselineFile::load_str(&s).unwrap();
        assert_eq!(parsed, f);
    }

    #[test]
    fn update_current_pushes_previous_into_history() {
        let mut f = BaselineFile::new();
        let first = make_baseline("aaa", 100, 5, datetime!(2026-06-01 12:00:00 UTC));
        let second = make_baseline("bbb", 200, 12, datetime!(2026-06-08 12:00:00 UTC));
        f.update_current("heartbeat-cadence", first.clone());
        f.update_current("heartbeat-cadence", second.clone());

        let entry = &f.scenarios["heartbeat-cadence"];
        assert_eq!(entry.current.as_ref(), Some(&second));
        assert_eq!(entry.history, vec![first]);
    }

    #[test]
    fn current_for_missing_scenario_returns_none() {
        let f = BaselineFile::new();
        assert!(f.current_for("nonexistent").is_none());
    }

    #[test]
    fn baseline_rate_zero_runs_is_zero() {
        let b = make_baseline("x", 0, 0, datetime!(2026-01-01 00:00:00 UTC));
        assert_eq!(b.rate(), 0.0);
    }

    #[test]
    fn baseline_rate_typical() {
        let b = make_baseline("x", 200, 12, datetime!(2026-01-01 00:00:00 UTC));
        assert!((b.rate() - 0.06).abs() < 1e-9);
    }

    #[test]
    fn build_hash_is_optional_in_toml() {
        // A baseline written without build_hash should parse cleanly.
        // Timestamps are stored as RFC 3339 strings (quoted), not as
        // native TOML datetimes — the `time` crate's serde adapter
        // expects strings, not the `toml::value::Datetime` map type.
        let toml = r#"
            [scenarios.heartbeat-cadence.current]
            commit = "abc"
            runs = 50
            failures = 3
            recorded_at = "2026-06-08T10:00:00Z"
        "#;
        let f = BaselineFile::load_str(toml).unwrap();
        let current = f.current_for("heartbeat-cadence").unwrap();
        assert!(current.build_hash.is_none());
        assert_eq!(current.runs, 50);
        assert_eq!(current.failures, 3);
    }

    #[test]
    fn build_hash_round_trips_when_present() {
        let mut f = BaselineFile::new();
        let mut b = make_baseline("abc", 50, 3, datetime!(2026-06-08 10:00:00 UTC));
        b.build_hash = Some("deadbeef".to_string());
        f.update_current("heartbeat-cadence", b);
        let s = f.to_string().unwrap();
        assert!(s.contains("build_hash = \"deadbeef\""));
        let parsed = BaselineFile::load_str(&s).unwrap();
        assert_eq!(parsed, f);
    }

    #[test]
    fn render_summary_flakes_only_filters_zero_failure_scenarios() {
        let mut f = BaselineFile::new();
        f.update_current(
            "clean-scenario",
            make_baseline("aaa", 100, 0, datetime!(2026-06-08 12:00:00 UTC)),
        );
        f.update_current(
            "flaky-scenario",
            make_baseline("bbb", 100, 3, datetime!(2026-06-08 12:00:00 UTC)),
        );
        let out = f.render_summary(SummaryOptions { flakes_only: true, ..Default::default() });
        assert!(out.contains("flaky-scenario"));
        assert!(!out.contains("clean-scenario"));
    }

    #[test]
    fn render_summary_flakes_only_sorts_by_lower_bound_descending() {
        let mut f = BaselineFile::new();
        // Three scenarios with the same observed rate (5%) but
        // different sample sizes. Larger N → narrower CI → higher
        // lower bound. So order should be c (200) > a (100) > b (50).
        f.update_current(
            "small-n", // 5/100, CI roughly [1.7%, 11.2%]
            make_baseline("a", 100, 5, datetime!(2026-06-08 12:00:00 UTC)),
        );
        f.update_current(
            "tiny-n", // 2.5/50 ≈ same rate, CI roughly [0.7%, 17%]
            make_baseline("b", 50, 2, datetime!(2026-06-08 12:00:00 UTC)),
        );
        f.update_current(
            "big-n", // 10/200, same rate, CI roughly [2.7%, 9.0%]
            make_baseline("c", 200, 10, datetime!(2026-06-08 12:00:00 UTC)),
        );
        let out = f.render_summary(SummaryOptions { flakes_only: true, ..Default::default() });
        let big_idx = out.find("big-n").expect("big-n present");
        let small_idx = out.find("small-n").expect("small-n present");
        let tiny_idx = out.find("tiny-n").expect("tiny-n present");
        // big-n has the tightest CI → highest lower bound → first.
        assert!(big_idx < small_idx, "big-n should sort above small-n");
        assert!(small_idx < tiny_idx, "small-n should sort above tiny-n");
    }

    #[test]
    fn render_summary_flakes_only_empty_when_no_flakes() {
        let mut f = BaselineFile::new();
        f.update_current(
            "clean",
            make_baseline("aaa", 100, 0, datetime!(2026-06-08 12:00:00 UTC)),
        );
        let out = f.render_summary(SummaryOptions { flakes_only: true, ..Default::default() });
        assert!(out.contains("no flaky scenarios"));
    }

    #[test]
    fn timing_fields_round_trip_when_present() {
        let mut f = BaselineFile::new();
        let mut b = make_baseline("abc", 50, 3, datetime!(2026-06-08 10:00:00 UTC));
        b.mean_duration_ms = Some(1234.5);
        b.p95_duration_ms = Some(1500.0);
        f.update_current("heartbeat-cadence", b);
        let s = f.to_string().unwrap();
        assert!(s.contains("mean_duration_ms = 1234.5"));
        assert!(s.contains("p95_duration_ms = 1500.0"));
        let parsed = BaselineFile::load_str(&s).unwrap();
        assert_eq!(parsed, f);
    }

    #[test]
    fn timing_fields_absent_in_serialized_form_when_none() {
        // No timing data → no mean/p95 lines in the TOML.
        let mut f = BaselineFile::new();
        f.update_current(
            "x",
            make_baseline("c", 50, 3, datetime!(2026-06-08 10:00:00 UTC)),
        );
        let s = f.to_string().unwrap();
        assert!(!s.contains("mean_duration_ms"));
        assert!(!s.contains("p95_duration_ms"));
    }

    #[test]
    fn render_summary_empty_file() {
        let f = BaselineFile::new();
        let out = f.render_summary(SummaryOptions::default());
        assert!(out.contains("(no scenarios recorded)"));
    }

    #[test]
    fn render_summary_current_only_omits_history_by_default() {
        let mut f = BaselineFile::new();
        f.update_current(
            "heartbeat-cadence",
            make_baseline("aaa", 100, 5, datetime!(2026-06-01 12:00:00 UTC)),
        );
        f.update_current(
            "heartbeat-cadence",
            make_baseline("bbb", 200, 12, datetime!(2026-06-08 12:00:00 UTC)),
        );
        let out = f.render_summary(SummaryOptions::default());
        // Current is shown.
        assert!(out.contains("12/200"));
        assert!(out.contains("at bbb"));
        // History is hidden.
        assert!(!out.contains("history:"));
        assert!(!out.contains("at aaa"));
    }

    #[test]
    fn render_summary_with_history_includes_previous_currents() {
        let mut f = BaselineFile::new();
        f.update_current(
            "heartbeat-cadence",
            make_baseline("aaa", 100, 5, datetime!(2026-06-01 12:00:00 UTC)),
        );
        f.update_current(
            "heartbeat-cadence",
            make_baseline("bbb", 200, 12, datetime!(2026-06-08 12:00:00 UTC)),
        );
        let out = f.render_summary(SummaryOptions { include_history: true, ..Default::default() });
        // Current
        assert!(out.contains("12/200  (6.0%, 95% CI ["));
        assert!(out.contains("at bbb"));
        // History (previous current pushed back)
        assert!(out.contains("history:"));
        assert!(out.contains("5/100  (5.0%, 95% CI ["));
        assert!(out.contains("at aaa"));
    }

    #[test]
    fn render_summary_shows_timing_when_present() {
        let mut f = BaselineFile::new();
        let mut b = make_baseline("abc", 50, 3, datetime!(2026-06-08 10:00:00 UTC));
        b.mean_duration_ms = Some(1234.0);
        b.p95_duration_ms = Some(1500.0);
        f.update_current("x", b);
        let out = f.render_summary(SummaryOptions::default());
        assert!(out.contains("timing: mean 1234ms, p95 1500ms"));
    }

    #[test]
    fn render_summary_omits_timing_when_absent() {
        let mut f = BaselineFile::new();
        f.update_current(
            "x",
            make_baseline("abc", 50, 3, datetime!(2026-06-08 10:00:00 UTC)),
        );
        let out = f.render_summary(SummaryOptions::default());
        assert!(!out.contains("timing"));
    }

    fn fresh_test_dir() -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let i = COUNTER.fetch_add(1, Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!(
            "itest-harness-baseline-test-{}-{i}",
            std::process::id()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn pending_path_appends_suffix() {
        let p = BaselineFile::pending_path_for(Path::new(".itest-baseline.toml"));
        assert_eq!(p.file_name().unwrap(), ".itest-baseline.toml.pending");
    }

    #[test]
    fn promote_pending_when_no_pending_returns_not_found() {
        let dir = fresh_test_dir();
        let canonical = dir.join("baseline.toml");
        let result = BaselineFile::promote_pending(&canonical);
        match result {
            Err(BaselineError::Io(e)) => assert_eq!(e.kind(), std::io::ErrorKind::NotFound),
            other => panic!("expected NotFound, got {other:?}"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn promote_pending_moves_current_strips_partial_archives_previous() {
        let dir = fresh_test_dir();
        let canonical = dir.join("baseline.toml");
        let pending = dir.join("baseline.toml.pending");

        // Previous canonical: one full run.
        let mut canonical_file = BaselineFile::new();
        canonical_file.update_current(
            "heartbeat-cadence",
            make_baseline("aaa", 200, 10, datetime!(2026-06-01 12:00:00 UTC)),
        );
        canonical_file.save_path(&canonical).unwrap();

        // Pending: a partial run that we'll promote.
        let mut pending_file = BaselineFile::new();
        let mut b = make_baseline("bbb", 487, 23, datetime!(2026-06-08 12:30:15 UTC));
        b.partial = Some(PartialMarker {
            requested_runs: 1000,
            interrupted_at: datetime!(2026-06-08 13:15:42 UTC),
            run_dir: None,
        });
        pending_file.update_current("heartbeat-cadence", b);
        pending_file.save_path(&pending).unwrap();

        let promoted = BaselineFile::promote_pending(&canonical).unwrap();
        let entry = &promoted.scenarios["heartbeat-cadence"];
        let current = entry.current.as_ref().unwrap();
        // Pending's data is now the canonical current.
        assert_eq!(current.commit, "bbb");
        assert_eq!(current.runs, 487);
        // Partial marker stripped on promotion.
        assert!(current.partial.is_none());
        // Previous canonical is in history.
        assert_eq!(entry.history.len(), 1);
        assert_eq!(entry.history[0].commit, "aaa");

        // Pending file removed.
        assert!(!pending.exists());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn promote_pending_with_no_existing_canonical_creates_one() {
        let dir = fresh_test_dir();
        let canonical = dir.join("baseline.toml");
        let pending = dir.join("baseline.toml.pending");
        let mut pending_file = BaselineFile::new();
        pending_file.update_current(
            "scn",
            make_baseline("abc", 50, 3, datetime!(2026-06-08 10:00:00 UTC)),
        );
        pending_file.save_path(&pending).unwrap();
        assert!(!canonical.exists());

        let _ = BaselineFile::promote_pending(&canonical).unwrap();
        assert!(canonical.exists());
        assert!(!pending.exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn discard_pending_removes_file_when_present() {
        let dir = fresh_test_dir();
        let canonical = dir.join("baseline.toml");
        let pending = dir.join("baseline.toml.pending");
        std::fs::write(&pending, "# stub\n").unwrap();
        BaselineFile::discard_pending(&canonical).unwrap();
        assert!(!pending.exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn discard_pending_when_absent_is_noop() {
        let dir = fresh_test_dir();
        let canonical = dir.join("baseline.toml");
        // No pending file exists; discard should not error.
        BaselineFile::discard_pending(&canonical).unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn partial_marker_absent_in_toml_by_default() {
        let mut f = BaselineFile::new();
        f.update_current(
            "heartbeat-cadence",
            make_baseline("abc", 50, 3, datetime!(2026-06-08 10:00:00 UTC)),
        );
        let s = f.to_string().unwrap();
        assert!(!s.contains("[scenarios.heartbeat-cadence.current.partial]"));
        assert!(!s.contains("requested_runs"));
    }

    #[test]
    fn partial_marker_round_trips_when_present() {
        let mut f = BaselineFile::new();
        let mut b = make_baseline("abc", 487, 23, datetime!(2026-06-08 12:30:15 UTC));
        b.partial = Some(PartialMarker {
            requested_runs: 1000,
            interrupted_at: datetime!(2026-06-08 13:15:42 UTC),
            run_dir: Some(".itest-runs/2026-06-08T12-30-15Z".to_string()),
        });
        f.update_current("heartbeat-cadence", b);
        let s = f.to_string().unwrap();
        // TOML shape — nested table, sane field names.
        assert!(s.contains("[scenarios.heartbeat-cadence.current.partial]"));
        assert!(s.contains("requested_runs = 1000"));
        assert!(s.contains("interrupted_at = \"2026-06-08T13:15:42Z\""));
        assert!(s.contains("run_dir = \".itest-runs/2026-06-08T12-30-15Z\""));

        let parsed = BaselineFile::load_str(&s).unwrap();
        assert_eq!(parsed, f);
    }

    #[test]
    fn old_baseline_files_parse_without_partial_field() {
        // A baseline file from before this field existed should still
        // parse cleanly, with `partial: None`.
        let toml = r#"
            [scenarios.heartbeat-cadence.current]
            commit = "abc"
            runs = 200
            failures = 12
            recorded_at = "2026-06-01T10:00:00Z"
        "#;
        let f = BaselineFile::load_str(toml).unwrap();
        let current = f.current_for("heartbeat-cadence").unwrap();
        assert!(current.partial.is_none());
    }

    #[test]
    fn timestamps_serialize_as_rfc3339() {
        let mut f = BaselineFile::new();
        f.update_current(
            "x",
            make_baseline("c", 1, 0, datetime!(2026-06-08 15:42:33 UTC)),
        );
        let s = f.to_string().unwrap();
        assert!(s.contains("2026-06-08T15:42:33Z"));
    }
}
