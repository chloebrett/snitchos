//! Per-iteration run history: tier 2 (`iterations.ndjson`) and the
//! tier-2.5 `metadata.toml` sidecar. Each `--repeat N` invocation
//! creates its own directory under the consumer-supplied
//! `history_root`. Files are append-only or write-once; the design
//! goal is "process dies, last completed observation is preserved."
//!
//! See `plans/itest-history-and-pending.md` for the broader rationale.

use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::serde::rfc3339;

/// One scenario-invocation row, serialised as a single JSON line in
/// `iterations.ndjson`. Field shape is the contract for tier H1's
/// metrics exporter.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IterationRow {
    /// 1-indexed iteration number — matches the runner's `--repeat`
    /// counter.
    pub iteration: u32,
    /// Scenario name.
    pub scenario: String,
    /// Wall-clock at the start of this scenario invocation. RFC 3339 UTC.
    #[serde(with = "rfc3339")]
    pub started_at: OffsetDateTime,
    /// Wall-clock elapsed for the scenario, milliseconds.
    pub duration_ms: u32,
    pub result: ResultKind,
    /// Scenario error string. Present only when `result == Fail`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Path (relative to the run directory) to the saved log file.
    /// Present only when `result == Fail` AND a log was captured.
    /// Step D wires this up; step C leaves it `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResultKind {
    Pass,
    Fail,
}

/// One-shot metadata about the run as a whole. Written once at run
/// start as `metadata.toml`; never modified afterward.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RunMetadata {
    pub run: RunMetadataInner,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RunMetadataInner {
    #[serde(with = "rfc3339")]
    pub started_at: OffsetDateTime,
    pub commit: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_hash: Option<String>,
    pub requested_repeat: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fail_fast: Option<u32>,
    pub scenarios: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
}

/// Compute the run-directory name. Format: UTC RFC-3339-style truncated
/// to second precision, with `:` swapped for `-` so it's filesystem-safe.
/// Collisions in practice are prevented by the integration-test lock
/// (one run at a time per checkout) plus the multi-second cargo build
/// startup, so we don't bother with nanoseconds.
///
/// Example: `2026-06-08T12-30-15Z`.
pub fn run_dir_name(started_at: OffsetDateTime) -> String {
    format!(
        "{:04}-{:02}-{:02}T{:02}-{:02}-{:02}Z",
        started_at.year(),
        started_at.month() as u8,
        started_at.day(),
        started_at.hour(),
        started_at.minute(),
        started_at.second(),
    )
}

/// Create a new run directory under `history_root`, write `metadata.toml`,
/// return both the directory path and an opened `HistoryWriter` ready
/// to append rows.
pub fn create_run_dir(history_root: &Path, metadata: &RunMetadata) -> io::Result<(PathBuf, HistoryWriter)> {
    let dir = history_root.join(run_dir_name(metadata.run.started_at));
    std::fs::create_dir_all(&dir)?;
    let meta_path = dir.join("metadata.toml");
    let toml_str =
        toml::to_string_pretty(metadata).map_err(|e| io::Error::other(e.to_string()))?;
    std::fs::write(&meta_path, toml_str)?;
    let writer = HistoryWriter::create(&dir)?;
    Ok((dir, writer))
}

/// Append-only handle to `iterations.ndjson`. `Drop` closes the file.
pub struct HistoryWriter {
    file: File,
}

impl HistoryWriter {
    pub fn create(run_dir: &Path) -> io::Result<Self> {
        let path = run_dir.join("iterations.ndjson");
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        Ok(Self { file })
    }

    /// Serialise `row` as one JSON object and append `\n`. Flushes
    /// best-effort (no `sync_all`); trades durability of the last
    /// 1-2 rows for negligible per-iteration cost. Document this
    /// trade-off in the module docs.
    pub fn append(&mut self, row: &IterationRow) -> io::Result<()> {
        let line = serde_json::to_string(row).map_err(io::Error::other)?;
        self.file.write_all(line.as_bytes())?;
        self.file.write_all(b"\n")?;
        self.file.flush()
    }
}

/// Streaming reader for `iterations.ndjson`. Yields one row at a time;
/// stops at EOF. Malformed lines bubble up as errors but don't
/// abort the iteration — caller decides whether to skip or stop.
pub fn read_iterations(path: &Path) -> io::Result<impl Iterator<Item = io::Result<IterationRow>>> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    Ok(reader.lines().filter_map(|line_res| match line_res {
        Ok(line) if line.trim().is_empty() => None,
        Ok(line) => Some(
            serde_json::from_str::<IterationRow>(&line).map_err(io::Error::other),
        ),
        Err(e) => Some(Err(e)),
    }))
}

/// Best-effort hostname read: checks `HOSTNAME` env var, returns
/// `None` if unset.
pub fn current_hostname() -> Option<String> {
    std::env::var("HOSTNAME").ok().filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use time::macros::datetime;

    fn fresh_test_dir() -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let i = COUNTER.fetch_add(1, Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!(
            "itest-harness-history-test-{}-{i}",
            std::process::id()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn run_dir_name_is_second_precision_filesystem_safe() {
        let name = run_dir_name(datetime!(2026-06-08 12:30:15 UTC));
        assert_eq!(name, "2026-06-08T12-30-15Z");
    }

    #[test]
    fn write_metadata_and_iterations_round_trips() {
        let root = fresh_test_dir();
        let metadata = RunMetadata {
            run: RunMetadataInner {
                started_at: datetime!(2026-06-08 12:30:15 UTC),
                commit: "abc1234".to_string(),
                build_hash: None,
                requested_repeat: 50,
                fail_fast: Some(3),
                scenarios: vec!["scn-a".to_string(), "scn-b".to_string()],
                hostname: None,
            },
        };
        let (run_dir, mut writer) = create_run_dir(&root, &metadata).unwrap();

        // metadata.toml round-trip
        let meta_path = run_dir.join("metadata.toml");
        let meta_str = std::fs::read_to_string(&meta_path).unwrap();
        let parsed: RunMetadata = toml::from_str(&meta_str).unwrap();
        assert_eq!(parsed, metadata);

        // Append two rows.
        writer
            .append(&IterationRow {
                iteration: 1,
                scenario: "scn-a".to_string(),
                started_at: datetime!(2026-06-08 12:30:16 UTC),
                duration_ms: 1247,
                result: ResultKind::Pass,
                error: None,
                log: None,
            })
            .unwrap();
        writer
            .append(&IterationRow {
                iteration: 1,
                scenario: "scn-b".to_string(),
                started_at: datetime!(2026-06-08 12:30:17 UTC),
                duration_ms: 2401,
                result: ResultKind::Fail,
                error: Some("scripted failure".to_string()),
                log: None,
            })
            .unwrap();
        drop(writer);

        // Read back via streaming reader.
        let ndjson_path = run_dir.join("iterations.ndjson");
        let rows: Vec<_> = read_iterations(&ndjson_path)
            .unwrap()
            .collect::<io::Result<_>>()
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].scenario, "scn-a");
        assert_eq!(rows[0].result, ResultKind::Pass);
        assert!(rows[0].error.is_none());
        assert_eq!(rows[1].scenario, "scn-b");
        assert_eq!(rows[1].result, ResultKind::Fail);
        assert_eq!(rows[1].error.as_deref(), Some("scripted failure"));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn ndjson_format_is_one_object_per_line() {
        let root = fresh_test_dir();
        let metadata = RunMetadata {
            run: RunMetadataInner {
                started_at: datetime!(2026-06-08 12:30:15 UTC),
                commit: "abc".to_string(),
                build_hash: None,
                requested_repeat: 1,
                fail_fast: None,
                scenarios: vec!["s".to_string()],
                hostname: None,
            },
        };
        let (run_dir, mut writer) = create_run_dir(&root, &metadata).unwrap();
        for i in 1..=3 {
            writer
                .append(&IterationRow {
                    iteration: i,
                    scenario: "s".to_string(),
                    started_at: datetime!(2026-06-08 12:30:15 UTC),
                    duration_ms: 100,
                    result: ResultKind::Pass,
                    error: None,
                    log: None,
                })
                .unwrap();
        }
        drop(writer);
        let raw = std::fs::read_to_string(run_dir.join("iterations.ndjson")).unwrap();
        // Three lines, each ending in `\n`, each starting with `{`.
        assert_eq!(raw.lines().count(), 3);
        assert!(raw.ends_with('\n'));
        for line in raw.lines() {
            assert!(line.starts_with('{') && line.ends_with('}'));
        }
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn read_iterations_skips_empty_lines() {
        let root = fresh_test_dir();
        let path = root.join("iterations.ndjson");
        std::fs::write(
            &path,
            "{\"iteration\":1,\"scenario\":\"x\",\"started_at\":\"2026-06-08T12:30:15Z\",\"duration_ms\":100,\"result\":\"pass\"}\n\n\n",
        )
        .unwrap();
        let rows: Vec<_> = read_iterations(&path)
            .unwrap()
            .collect::<io::Result<_>>()
            .unwrap();
        assert_eq!(rows.len(), 1);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn read_iterations_surfaces_malformed_line_as_error() {
        let root = fresh_test_dir();
        let path = root.join("iterations.ndjson");
        std::fs::write(&path, "not json\n").unwrap();
        let rows: Vec<io::Result<IterationRow>> = read_iterations(&path).unwrap().collect();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].is_err());
        std::fs::remove_dir_all(&root).ok();
    }
}
