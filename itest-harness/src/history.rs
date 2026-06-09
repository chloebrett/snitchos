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
pub(crate) struct IterationRow {
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
    /// Cause-bucket this failure was attributed to. Present only when
    /// `result == Fail` and the harness captured enough evidence to
    /// classify. Drives the per-bucket flake-rate breakdown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<crate::signature::Signature>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ResultKind {
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
    /// Wfi-batch worker count (`--jobs`). Records the parallelism a run
    /// executed under, so a failure can be reproduced / its host
    /// contention reasoned about. `None` for metadata written before
    /// this field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jobs: Option<u32>,
    /// Cpu-batch worker count (`--cpu-jobs`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_jobs: Option<u32>,
    /// The full command line that launched the run, for exact repro.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invocation: Option<String>,
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
pub(crate) fn create_run_dir(history_root: &Path, metadata: &RunMetadata) -> io::Result<(PathBuf, HistoryWriter)> {
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
pub(crate) struct HistoryWriter {
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
pub(crate) fn read_iterations(path: &Path) -> io::Result<impl Iterator<Item = io::Result<IterationRow>>> {
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

/// Persist a failed iteration's structured `FailureCapture` next to its
/// `fail-*.log`, as `fail-<scenario>-<iteration>.capture.json`. Returns
/// the bare filename (for traceability in the iteration row). This is
/// the structured-telemetry counterpart to the UART log: the frame
/// transcript, histogram, and per-hart timestamps the classifier and a
/// human debugger both need, which the `.log` does not carry.
pub(crate) fn write_capture_sidecar(
    run_dir: &Path,
    scenario: &str,
    iteration: u32,
    capture: &crate::signature::FailureCapture,
) -> io::Result<String> {
    let name = format!("fail-{scenario}-{iteration}.capture.json");
    let json = serde_json::to_string_pretty(capture).map_err(io::Error::other)?;
    std::fs::write(run_dir.join(&name), json)?;
    Ok(name)
}

/// Best-effort hostname read: checks `HOSTNAME` env var, returns
/// `None` if unset.
pub(crate) fn current_hostname() -> Option<String> {
    std::env::var("HOSTNAME").ok().filter(|s| !s.is_empty())
}

/// Per-scenario stats reconstructed from `iterations.ndjson`. Mirrors
/// the fields a `Baseline` cares about, minus identity (commit /
/// `recorded_at` — those come from `metadata.toml`).
#[derive(Debug, Clone, PartialEq)]
pub struct RecoveredScenario {
    pub runs: u32,
    pub failures: u32,
    pub mean_duration_ms: Option<f64>,
    pub p95_duration_ms: Option<f64>,
    /// This scenario's failure counts by cause-bucket. Failures with no
    /// recorded signature count as `Unknown`.
    pub signature_counts: std::collections::BTreeMap<crate::signature::Signature, u32>,
}

/// Full result of `aggregate_run_dir`: the run-level `metadata.toml`
/// plus per-scenario stats derived from streaming the NDJSON.
#[derive(Debug, Clone, PartialEq)]
pub struct RecoveredRun {
    pub metadata: RunMetadata,
    pub scenarios: std::collections::BTreeMap<String, RecoveredScenario>,
    /// Suite-wide failure counts by cause-bucket across the run. Failures
    /// with no recorded signature (older rows) count as `Unknown`.
    pub signature_counts: std::collections::BTreeMap<crate::signature::Signature, u32>,
}

/// Rebuild per-scenario stats by streaming `iterations.ndjson`. Used by
/// `baseline recover`: if the pending sidecar is lost (process killed
/// before the runner could write it), the NDJSON has every iteration
/// we observed and we can reconstruct the partial baseline from it.
///
/// Malformed lines are skipped with a warning to stderr rather than
/// failing the recovery — better partial-recovery than no-recovery.
pub fn aggregate_run_dir(run_dir: &Path) -> io::Result<RecoveredRun> {
    type SigMap = std::collections::BTreeMap<crate::signature::Signature, u32>;

    let meta_str = std::fs::read_to_string(run_dir.join("metadata.toml"))?;
    let metadata: RunMetadata =
        toml::from_str(&meta_str).map_err(|e| io::Error::other(e.to_string()))?;

    let mut runs_per: std::collections::BTreeMap<String, u32> = std::collections::BTreeMap::new();
    let mut fails_per: std::collections::BTreeMap<String, u32> = std::collections::BTreeMap::new();
    let mut durations: std::collections::BTreeMap<String, Vec<u32>> =
        std::collections::BTreeMap::new();
    let mut signature_counts: SigMap = SigMap::new();
    let mut signatures_per: std::collections::BTreeMap<String, SigMap> =
        std::collections::BTreeMap::new();

    for row in read_iterations(&run_dir.join("iterations.ndjson"))? {
        let row = match row {
            Ok(r) => r,
            Err(e) => {
                eprintln!("warning: skipping malformed iteration row: {e}");
                continue;
            }
        };
        *runs_per.entry(row.scenario.clone()).or_insert(0) += 1;
        if row.result == ResultKind::Fail {
            *fails_per.entry(row.scenario.clone()).or_insert(0) += 1;
            let sig = row.signature.unwrap_or(crate::signature::Signature::Unknown);
            *signature_counts.entry(sig).or_insert(0) += 1;
            *signatures_per
                .entry(row.scenario.clone())
                .or_default()
                .entry(sig)
                .or_insert(0) += 1;
        }
        durations
            .entry(row.scenario)
            .or_default()
            .push(row.duration_ms);
    }

    let scenarios = runs_per
        .into_iter()
        .map(|(name, runs)| {
            let failures = fails_per.get(&name).copied().unwrap_or(0);
            let durs = durations.remove(&name).unwrap_or_default();
            let (mean_ms, p95_ms) = summarise_durations(&durs);
            let signature_counts = signatures_per.remove(&name).unwrap_or_default();
            (
                name,
                RecoveredScenario {
                    runs,
                    failures,
                    mean_duration_ms: mean_ms,
                    p95_duration_ms: p95_ms,
                    signature_counts,
                },
            )
        })
        .collect();

    Ok(RecoveredRun {
        metadata,
        scenarios,
        signature_counts,
    })
}

/// Result of `prune_runs`: which directories were retained vs removed.
/// Names are bare directory names (not full paths) so they round-trip
/// through user-facing output cleanly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PruneReport {
    pub kept: Vec<String>,
    pub removed: Vec<String>,
}

/// Keep the most-recent `keep_last` run directories under `history_root`
/// and remove the rest. Directory names are sorted lexicographically —
/// safe because `run_dir_name` emits ISO-8601 timestamps that sort the
/// same as their chronological order.
///
/// `keep_last == 0` removes every run directory. Non-directory entries
/// and entries whose names don't match the timestamp format are left
/// untouched — caller's manual files don't get nuked.
///
/// Returns kept/removed names in chronological order (oldest first).
/// Missing `history_root` returns an empty report rather than an error
/// — pruning a never-used checkout is a no-op, not a failure.
pub fn prune_runs(history_root: &Path, keep_last: usize) -> io::Result<PruneReport> {
    if !history_root.exists() {
        return Ok(PruneReport { kept: Vec::new(), removed: Vec::new() });
    }
    let mut names: Vec<String> = Vec::new();
    for entry in std::fs::read_dir(history_root)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if !looks_like_run_dir_name(&name) {
            continue;
        }
        names.push(name);
    }
    names.sort();
    let total = names.len();
    let cut = total.saturating_sub(keep_last);
    let (to_remove, to_keep) = names.split_at(cut);
    for name in to_remove {
        std::fs::remove_dir_all(history_root.join(name))?;
    }
    Ok(PruneReport {
        kept: to_keep.to_vec(),
        removed: to_remove.to_vec(),
    })
}

/// Recognise our generated `YYYY-MM-DDTHH-MM-SSZ` shape — guards
/// against `prune_runs` deleting user-placed files inside `.itest-runs/`.
fn looks_like_run_dir_name(name: &str) -> bool {
    // Length 20, ends in 'Z', positions per the format string in
    // `run_dir_name`.
    let bytes = name.as_bytes();
    bytes.len() == 20
        && bytes[19] == b'Z'
        && bytes[10] == b'T'
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes[13] == b'-'
        && bytes[16] == b'-'
}

fn summarise_durations(durs: &[u32]) -> (Option<f64>, Option<f64>) {
    if durs.is_empty() {
        return (None, None);
    }
    let mean = durs.iter().map(|&d| f64::from(d)).sum::<f64>() / durs.len() as f64;
    let mut sorted: Vec<u32> = durs.to_vec();
    sorted.sort_unstable();
    // Nearest-rank p95, 1-indexed: ceil(0.95 * n).
    let rank = ((0.95 * sorted.len() as f64).ceil() as usize).max(1);
    let p95 = f64::from(sorted[rank - 1]);
    (Some(mean), Some(p95))
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
                jobs: None,
                cpu_jobs: None,
                invocation: None,
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
                signature: None,
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
                signature: None,
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
    fn write_capture_sidecar_persists_round_trippable_json() {
        use crate::signature::{FailureCapture, WaitOutcome};
        let root = fresh_test_dir();
        let cap = FailureCapture {
            outcome: Some(WaitOutcome::Timeout),
            frames_seen: 12,
            transcript: vec!["Hello { .. }".into(), "SpanStart { .. }".into()],
            ..Default::default()
        };
        let name = write_capture_sidecar(&root, "kernel-heap-metrics", 9, &cap).unwrap();
        assert_eq!(name, "fail-kernel-heap-metrics-9.capture.json");

        let raw = std::fs::read_to_string(root.join(&name)).unwrap();
        let back: FailureCapture = serde_json::from_str(&raw).unwrap();
        assert_eq!(back, cap);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn iteration_row_carries_failure_signature() {
        let row = IterationRow {
            iteration: 9,
            scenario: "kernel-heap-metrics".to_string(),
            started_at: datetime!(2026-06-08 12:30:15 UTC),
            duration_ms: 30_001,
            result: ResultKind::Fail,
            error: Some("no snitchos.heap.alloc_total within 30s".to_string()),
            log: Some("fail-kernel-heap-metrics-9.log".to_string()),
            signature: Some(crate::signature::Signature::BudgetExhausted),
        };
        let json = serde_json::to_value(&row).unwrap();
        assert_eq!(json["signature"], "budget_exhausted");

        let back: IterationRow = serde_json::from_value(json).unwrap();
        assert_eq!(back, row);

        // A passing row omits the signature entirely.
        let pass = IterationRow {
            signature: None,
            ..row
        };
        let pass_json = serde_json::to_string(&pass).unwrap();
        assert!(!pass_json.contains("signature"));
    }

    #[test]
    fn metadata_records_parallelism_and_invocation() {
        let meta = RunMetadata {
            run: RunMetadataInner {
                started_at: datetime!(2026-06-08 12:30:15 UTC),
                commit: "abc1234".to_string(),
                build_hash: None,
                requested_repeat: 50,
                fail_fast: None,
                scenarios: vec!["s".to_string()],
                hostname: None,
                jobs: Some(10),
                cpu_jobs: Some(3),
                invocation: Some("xtask itest --jobs 10 --cpu-jobs 3".to_string()),
            },
        };
        let toml_str = toml::to_string_pretty(&meta).unwrap();
        assert!(toml_str.contains("jobs = 10"));
        assert!(toml_str.contains("cpu_jobs = 3"));
        assert!(toml_str.contains("xtask itest --jobs 10 --cpu-jobs 3"));
        let back: RunMetadata = toml::from_str(&toml_str).unwrap();
        assert_eq!(back, meta);
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
                jobs: None,
                cpu_jobs: None,
                invocation: None,
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
                    signature: None,
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
    fn aggregate_run_dir_reconstructs_per_scenario_stats() {
        let root = fresh_test_dir();
        let metadata = RunMetadata {
            run: RunMetadataInner {
                started_at: datetime!(2026-06-08 12:30:15 UTC),
                commit: "abc1234".to_string(),
                build_hash: Some("deadbeef".to_string()),
                requested_repeat: 5,
                fail_fast: None,
                scenarios: vec!["scn-a".to_string(), "scn-b".to_string()],
                hostname: None,
                jobs: None,
                cpu_jobs: None,
                invocation: None,
            },
        };
        let (run_dir, mut writer) = create_run_dir(&root, &metadata).unwrap();
        // scn-a: 3 runs, 1 failure. scn-b: 2 runs, 0 failures.
        for (iter, scn, result, dur) in [
            (1, "scn-a", ResultKind::Pass, 100),
            (1, "scn-b", ResultKind::Pass, 200),
            (2, "scn-a", ResultKind::Fail, 150),
            (2, "scn-b", ResultKind::Pass, 220),
            (3, "scn-a", ResultKind::Pass, 110),
        ] {
            writer
                .append(&IterationRow {
                    iteration: iter,
                    scenario: scn.to_string(),
                    started_at: datetime!(2026-06-08 12:30:15 UTC),
                    duration_ms: dur,
                    result,
                    error: if result == ResultKind::Fail {
                        Some("synthetic".into())
                    } else {
                        None
                    },
                    log: None,
                    signature: None,
                })
                .unwrap();
        }
        drop(writer);

        let recovered = aggregate_run_dir(&run_dir).unwrap();
        assert_eq!(recovered.metadata, metadata);
        let a = recovered.scenarios.get("scn-a").unwrap();
        assert_eq!(a.runs, 3);
        assert_eq!(a.failures, 1);
        assert!((a.mean_duration_ms.unwrap() - 120.0).abs() < 1e-6);
        // p95 of [100, 110, 150] is 150 (nearest-rank).
        assert_eq!(a.p95_duration_ms, Some(150.0));
        let b = recovered.scenarios.get("scn-b").unwrap();
        assert_eq!(b.runs, 2);
        assert_eq!(b.failures, 0);

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn aggregate_run_dir_tracks_per_scenario_signatures() {
        use crate::signature::Signature;
        let root = fresh_test_dir();
        let metadata = RunMetadata {
            run: RunMetadataInner {
                started_at: datetime!(2026-06-08 12:30:15 UTC),
                commit: "abc1234".to_string(),
                build_hash: None,
                requested_repeat: 4,
                fail_fast: None,
                scenarios: vec!["s".to_string()],
                hostname: None,
                jobs: None,
                cpu_jobs: None,
                invocation: None,
            },
        };
        let (run_dir, mut writer) = create_run_dir(&root, &metadata).unwrap();
        for (result, sig) in [
            (ResultKind::Fail, Some(Signature::Wedge)),
            (ResultKind::Fail, Some(Signature::Wedge)),
            (ResultKind::Fail, Some(Signature::BudgetExhausted)),
            (ResultKind::Pass, None),
        ] {
            writer
                .append(&IterationRow {
                    iteration: 1,
                    scenario: "s".to_string(),
                    started_at: datetime!(2026-06-08 12:30:15 UTC),
                    duration_ms: 100,
                    result,
                    error: None,
                    log: None,
                    signature: sig,
                })
                .unwrap();
        }
        drop(writer);

        let recovered = aggregate_run_dir(&run_dir).unwrap();
        let sc = &recovered.scenarios["s"].signature_counts;
        assert_eq!(sc.get(&Signature::Wedge), Some(&2));
        assert_eq!(sc.get(&Signature::BudgetExhausted), Some(&1));
        assert_eq!(sc.values().sum::<u32>(), 3);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn aggregate_run_dir_counts_signatures_for_failures_only() {
        use crate::signature::Signature;
        let root = fresh_test_dir();
        let metadata = RunMetadata {
            run: RunMetadataInner {
                started_at: datetime!(2026-06-08 12:30:15 UTC),
                commit: "abc1234".to_string(),
                build_hash: None,
                requested_repeat: 5,
                fail_fast: None,
                scenarios: vec!["s".to_string()],
                hostname: None,
                jobs: None,
                cpu_jobs: None,
                invocation: None,
            },
        };
        let (run_dir, mut writer) = create_run_dir(&root, &metadata).unwrap();
        for (scn, result, sig) in [
            ("a", ResultKind::Fail, Some(Signature::Wedge)),
            ("b", ResultKind::Fail, Some(Signature::Wedge)),
            ("c", ResultKind::Fail, Some(Signature::BudgetExhausted)),
            ("d", ResultKind::Fail, None), // unclassified fail → Unknown
            ("e", ResultKind::Pass, None), // a pass must NOT be counted
        ] {
            writer
                .append(&IterationRow {
                    iteration: 1,
                    scenario: scn.to_string(),
                    started_at: datetime!(2026-06-08 12:30:15 UTC),
                    duration_ms: 100,
                    result,
                    error: None,
                    log: None,
                    signature: sig,
                })
                .unwrap();
        }
        drop(writer);

        let recovered = aggregate_run_dir(&run_dir).unwrap();
        let sc = &recovered.signature_counts;
        assert_eq!(sc.get(&Signature::Wedge), Some(&2));
        assert_eq!(sc.get(&Signature::BudgetExhausted), Some(&1));
        assert_eq!(sc.get(&Signature::Unknown), Some(&1));
        assert_eq!(sc.values().sum::<u32>(), 4); // the pass is excluded
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn prune_runs_keeps_most_recent_n_and_removes_older() {
        let root = fresh_test_dir();
        // Create five run dirs with distinct (and sortable) timestamps.
        let names = [
            "2026-06-01T12-00-00Z",
            "2026-06-02T12-00-00Z",
            "2026-06-03T12-00-00Z",
            "2026-06-04T12-00-00Z",
            "2026-06-05T12-00-00Z",
        ];
        for n in names {
            std::fs::create_dir_all(root.join(n)).unwrap();
            std::fs::write(root.join(n).join("placeholder"), b"x").unwrap();
        }
        let report = prune_runs(&root, 2).unwrap();
        assert_eq!(
            report.kept,
            vec![
                "2026-06-04T12-00-00Z".to_string(),
                "2026-06-05T12-00-00Z".to_string(),
            ]
        );
        assert_eq!(
            report.removed,
            vec![
                "2026-06-01T12-00-00Z".to_string(),
                "2026-06-02T12-00-00Z".to_string(),
                "2026-06-03T12-00-00Z".to_string(),
            ]
        );
        for n in &report.removed {
            assert!(!root.join(n).exists());
        }
        for n in &report.kept {
            assert!(root.join(n).exists());
        }
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn prune_runs_ignores_non_run_dir_entries() {
        let root = fresh_test_dir();
        std::fs::create_dir_all(root.join("2026-06-01T12-00-00Z")).unwrap();
        std::fs::write(root.join("README"), b"hi").unwrap();
        std::fs::create_dir_all(root.join("notes")).unwrap();
        let report = prune_runs(&root, 0).unwrap();
        assert_eq!(report.kept, Vec::<String>::new());
        assert_eq!(report.removed, vec!["2026-06-01T12-00-00Z".to_string()]);
        assert!(root.join("README").exists());
        assert!(root.join("notes").exists());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn prune_runs_missing_root_returns_empty_report() {
        let path = std::env::temp_dir().join(format!(
            "itest-harness-prune-missing-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        let report = prune_runs(&path, 5).unwrap();
        assert!(report.kept.is_empty());
        assert!(report.removed.is_empty());
    }

    #[test]
    fn prune_runs_keep_more_than_exists_is_noop() {
        let root = fresh_test_dir();
        std::fs::create_dir_all(root.join("2026-06-01T12-00-00Z")).unwrap();
        let report = prune_runs(&root, 10).unwrap();
        assert_eq!(report.removed, Vec::<String>::new());
        assert_eq!(report.kept, vec!["2026-06-01T12-00-00Z".to_string()]);
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
