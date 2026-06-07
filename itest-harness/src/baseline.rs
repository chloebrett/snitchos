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
use std::path::Path;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::serde::rfc3339;

/// A single baseline measurement for one scenario.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScenarioBaseline {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<Baseline>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<Baseline>,
}

/// Root document of the baseline TOML.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
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
