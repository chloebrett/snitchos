//! Baseline / run-history management — the implementation behind
//! `cargo xtask baseline {show,promote,discard,recover,adopt,prune,export,push}`.
//!
//! Everything here operates on the two flake-tracking artifacts: the canonical
//! `.itest-baseline.toml` (and its `.pending` sidecar) and the per-run history
//! under `.itest-runs/`. The actual file/stat logic lives in `itest_harness`;
//! these are the user-facing wrappers (messaging + exit codes). The test-run
//! path lives in the parent `itest` module.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use itest_harness::{
    BaselineFile, SummaryOptions, aggregate_run_dir, prune_runs as prune_runs_in, push_otlp,
    push_otlp_with_timeout, render_prometheus, write_atomic,
};

use super::{BASELINE_PATH, DEFAULT_OTLP_ENDPOINT, HISTORY_ROOT};

/// Wall-clock nanoseconds since the epoch, clamped to `u64`. One timestamp per
/// OTLP push batch — every data point is an observation of the same baseline at
/// the same instant.
pub(crate) fn now_unix_nanos() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos().min(u128::from(u64::MAX)) as u64)
}

/// Load the canonical baseline and push it to `endpoint` as OTLP/HTTP metrics.
/// `timeouts` = `Some((connect, read))` uses the timeout-bounded path (for the
/// best-effort end-of-run auto-push); `None` uses the unbounded path (the
/// explicit one-shot `baseline push`). Returns `Ok(None)` when there's no
/// baseline file to push, `Ok(Some((http_status, scenario_count)))` on a
/// completed exchange, `Err` on a parse or transport failure. Shared by
/// `push_otlp_metrics` here and `super::try_auto_push`.
pub(crate) fn load_and_push(
    endpoint: &str,
    timeouts: Option<(Duration, Duration)>,
) -> Result<Option<(u16, usize)>, String> {
    let baseline_path = Path::new(BASELINE_PATH);
    if !baseline_path.exists() {
        return Ok(None);
    }
    let file =
        BaselineFile::load_path(baseline_path).map_err(|e| format!("failed to parse {BASELINE_PATH}: {e}"))?;
    let now_ns = now_unix_nanos();
    let status = match timeouts {
        Some((connect, read)) => {
            push_otlp_with_timeout(endpoint, &file, now_ns, Some(connect), Some(read))
        }
        None => push_otlp(endpoint, &file, now_ns),
    }
    .map_err(|e| e.to_string())?;
    Ok(Some((status, file.scenarios.len())))
}

/// Promote `.itest-baseline.toml.pending` into the canonical baseline
/// file. Wraps `BaselineFile::promote_pending` with user-facing
/// messaging and `baseline show`-friendly exit codes.
pub fn promote_pending() -> ExitCode {
    let canonical = Path::new(BASELINE_PATH);
    let pending = BaselineFile::pending_path_for(canonical);
    if !pending.exists() {
        eprintln!("no pending baseline at {}", pending.display());
        return ExitCode::from(1);
    }
    match BaselineFile::promote_pending(canonical) {
        Ok(_) => {
            eprintln!(
                "Promoted {} → {} (previous current pushed to history).",
                pending.display(),
                canonical.display()
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("promote failed: {e}");
            ExitCode::from(1)
        }
    }
}

/// Delete the pending baseline sidecar if present. Idempotent.
pub fn discard_pending() -> ExitCode {
    let canonical = Path::new(BASELINE_PATH);
    let pending = BaselineFile::pending_path_for(canonical);
    let existed = pending.exists();
    match BaselineFile::discard_pending(canonical) {
        Ok(()) => {
            if existed {
                eprintln!("Discarded {}.", pending.display());
            } else {
                eprintln!("No pending baseline to discard ({}).", pending.display());
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("discard failed: {e}");
            ExitCode::from(1)
        }
    }
}

/// One-shot push of the canonical baseline as OTLP/HTTP metrics to
/// `endpoint`, defaulting to [`DEFAULT_OTLP_ENDPOINT`] (the bundled stack's
/// Prometheus receiver) when `None`. Endpoint should be the OTLP receiver
/// root, e.g. `http://localhost:9090/api/v1/otlp` for Prometheus with
/// `--web.enable-otlp-receiver`, or `http://localhost:4318` for the
/// `OTel` collector default. `/v1/metrics` is appended automatically.
pub fn push_otlp_metrics(endpoint: Option<&str>) -> ExitCode {
    let endpoint = endpoint.unwrap_or(DEFAULT_OTLP_ENDPOINT);
    match load_and_push(endpoint, None) {
        Ok(None) => {
            eprintln!("no baseline file at {BASELINE_PATH}; nothing to push");
            ExitCode::SUCCESS
        }
        Ok(Some((status, scenarios))) if (200..300).contains(&status) => {
            eprintln!("Pushed {scenarios} scenarios to {endpoint} (HTTP {status})");
            ExitCode::SUCCESS
        }
        Ok(Some((status, _))) => {
            eprintln!("OTLP receiver returned HTTP {status} from {endpoint}");
            ExitCode::from(1)
        }
        Err(e) => {
            eprintln!("OTLP push to {endpoint} failed: {e}");
            ExitCode::from(1)
        }
    }
}

/// Render the canonical baseline file as Prometheus textfile-format
/// metrics at `out_path`. Designed for `node_exporter --collector.textfile`
/// scraping. Atomic write — half-rendered files never appear on disk.
/// Exits 0 if the baseline file is absent (empty export is valid; an
/// empty `.prom` file is also valid).
pub fn export_prom(out_path: &Path) -> ExitCode {
    let baseline_path = Path::new(BASELINE_PATH);
    let file = if baseline_path.exists() {
        match BaselineFile::load_path(baseline_path) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("failed to parse {BASELINE_PATH}: {e}");
                return ExitCode::from(1);
            }
        }
    } else {
        BaselineFile::new()
    };
    let body = render_prometheus(&file);
    if let Err(e) = write_atomic(out_path, &body) {
        eprintln!("failed to write {}: {e}", out_path.display());
        return ExitCode::from(1);
    }
    eprintln!(
        "Wrote {} ({} scenarios)",
        out_path.display(),
        file.scenarios.len()
    );
    ExitCode::SUCCESS
}

/// Prune `.itest-runs/` to the most-recent `keep_last` run directories.
/// Older ones are removed wholesale (NDJSON, metadata, captured logs).
/// Exit 0 always on success, including the no-op case.
pub fn prune_runs(keep_last: usize) -> ExitCode {
    let root = PathBuf::from(HISTORY_ROOT);
    match prune_runs_in(&root, keep_last) {
        Ok(report) => {
            if report.removed.is_empty() {
                eprintln!(
                    "No runs removed ({} kept under {}).",
                    report.kept.len(),
                    root.display()
                );
            } else {
                eprintln!(
                    "Removed {} run(s) from {} (kept {} most-recent):",
                    report.removed.len(),
                    root.display(),
                    report.kept.len()
                );
                for n in &report.removed {
                    eprintln!("  - {n}");
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("prune failed: {e}");
            ExitCode::from(1)
        }
    }
}

/// Find the most-recent run directory under `HISTORY_ROOT`. Returns
/// `None` if the root doesn't exist or has no entries matching the
/// `YYYY-MM-DDTHH-MM-SSZ` shape generated by `history::run_dir_name`.
/// ISO-8601 timestamps sort chronologically, so lexicographic max =
/// most recent.
fn find_most_recent_run_dir() -> Option<PathBuf> {
    let root = Path::new(HISTORY_ROOT);
    if !root.exists() {
        return None;
    }
    let mut candidates: Vec<PathBuf> = std::fs::read_dir(root)
        .ok()?
        .filter_map(std::result::Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .filter(|p| {
            // Same shape check as itest_harness::prune_runs:
            // length 20, ends in 'Z', positional separators match.
            let Some(name) = p.file_name().and_then(|s| s.to_str()) else {
                return false;
            };
            let b = name.as_bytes();
            b.len() == 20
                && b[19] == b'Z'
                && b[10] == b'T'
                && b[4] == b'-'
                && b[7] == b'-'
                && b[13] == b'-'
                && b[16] == b'-'
        })
        .collect();
    candidates.sort();
    candidates.pop()
}

/// Retroactively adopt a completed run as the new canonical baseline.
/// `run_dir` is the explicit directory, or `None` to pick the most
/// recent under `.itest-runs/`. The previous canonical `current` per
/// scenario is pushed to `history`. No partial marker — adoption is
/// a deliberate "promote this run."
pub fn adopt_run(run_dir: Option<PathBuf>) -> ExitCode {
    let canonical = Path::new(BASELINE_PATH);
    let run_dir = match run_dir {
        Some(p) => p,
        None => if let Some(p) = find_most_recent_run_dir() { p } else {
            eprintln!(
                "no run directories found under {HISTORY_ROOT}/ — \
                 run `cargo xtask itest` at least once first."
            );
            return ExitCode::from(1);
        },
    };
    if !run_dir.exists() {
        eprintln!("run directory does not exist: {}", run_dir.display());
        return ExitCode::from(1);
    }
    let recovered = match aggregate_run_dir(&run_dir) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("failed to aggregate run directory: {e}");
            return ExitCode::from(1);
        }
    };
    // Load existing canonical (preserves per-scenario history beyond
    // the current row) or start fresh if absent.
    let mut file = if canonical.exists() {
        match BaselineFile::load_path(canonical) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("failed to parse existing {BASELINE_PATH}: {e}");
                return ExitCode::from(1);
            }
        }
    } else {
        BaselineFile::new()
    };
    file.adopt_recovered(&recovered);
    if let Err(e) = file.save_path(canonical) {
        eprintln!("failed to write {BASELINE_PATH}: {e}");
        return ExitCode::from(1);
    }
    eprintln!(
        "Adopted {} as the new canonical baseline ({} scenarios). \
         Previous current entries pushed to history.",
        run_dir.display(),
        recovered.scenarios.len(),
    );
    ExitCode::SUCCESS
}

/// Rebuild the pending baseline sidecar from a per-run history
/// directory's NDJSON. Used when the in-process pending write was
/// lost (process killed before the runner could write it, disk full
/// at the wrong moment, etc.). Refuses if a pending file already
/// exists — caller should `baseline discard` or `baseline promote`
/// first, then re-run recovery.
pub fn recover_pending(run_dir: &Path) -> ExitCode {
    let canonical = Path::new(BASELINE_PATH);
    let pending = BaselineFile::pending_path_for(canonical);
    if pending.exists() {
        eprintln!(
            "refusing to overwrite existing pending baseline at {}.\n\
             Promote (`baseline promote`) or discard (`baseline discard`) it first.",
            pending.display()
        );
        return ExitCode::from(1);
    }
    if !run_dir.exists() {
        eprintln!("run directory does not exist: {}", run_dir.display());
        return ExitCode::from(1);
    }
    let recovered = match aggregate_run_dir(run_dir) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("failed to aggregate run directory: {e}");
            return ExitCode::from(1);
        }
    };
    let run_dir_name = run_dir
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();
    let file = BaselineFile::from_recovered(&recovered, &run_dir_name);
    if let Err(e) = file.save_path(&pending) {
        eprintln!("failed to write pending baseline: {e}");
        return ExitCode::from(1);
    }
    eprintln!(
        "Recovered pending baseline from {} → {}.\n\
         {} scenarios reconstructed. Inspect with `baseline show`, then\n\
         `baseline promote` or `baseline discard`.",
        run_dir.display(),
        pending.display(),
        recovered.scenarios.len(),
    );
    ExitCode::SUCCESS
}

/// Load `.itest-baseline.toml` and print its rendered summary. Exits
/// with `0` on success (including "file doesn't exist" — that's a
/// valid initial state). Returns `1` only on parse error.
///
/// If `pending` is set, render the `.pending` sidecar instead — useful
/// for inspecting a partial baseline before deciding to
/// `baseline promote` or `baseline discard`. When `pending` is
/// unset and a pending sidecar exists, surface a banner at the top
/// of the canonical summary so the user knows partial work is waiting.
pub fn show_baseline(include_history: bool, flakes_only: bool, pending: bool) -> ExitCode {
    let canonical = Path::new(BASELINE_PATH);
    let pending_path = BaselineFile::pending_path_for(canonical);

    let (path_to_show, label) = if pending {
        (pending_path.clone(), "pending sidecar")
    } else {
        (canonical.to_path_buf(), "canonical baseline")
    };

    if !path_to_show.exists() {
        if pending {
            eprintln!("no pending baseline at {}", path_to_show.display());
        } else {
            eprintln!("no baseline file at {BASELINE_PATH}");
        }
        return ExitCode::SUCCESS;
    }

    // When showing the canonical file and a pending sidecar exists,
    // banner it. The user almost always wants to know.
    if !pending && pending_path.exists() {
        eprintln!(
            "NOTE: pending baseline present at {} — inspect with `baseline show --pending`,\n\
             then promote (`baseline promote`) or discard (`baseline discard`).\n",
            pending_path.display()
        );
    }

    match BaselineFile::load_path(&path_to_show) {
        Ok(file) => {
            eprintln!("=== {} ({label}) ===\n", path_to_show.display());
            eprint!(
                "{}",
                file.render_summary(SummaryOptions {
                    include_history,
                    flakes_only,
                })
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("failed to parse {}: {e}", path_to_show.display());
            ExitCode::from(1)
        }
    }
}
