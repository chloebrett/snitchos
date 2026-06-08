//! Kernel integration tests. Each scenario boots the kernel in QEMU,
//! reads frames off the virtio-console socket, and asserts on the
//! decoded `Frame` sequence. See `plans/kernel-integration-tests.md`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};

use itest_harness::{
    BaselineFile, CpuProfile, ItestLock, LockError, RunnerConfig, SummaryOptions,
    aggregate_run_dir, prune_runs as prune_runs_in, push_otlp, push_otlp_with_timeout,
    render_prometheus, write_atomic,
};

use crate::qemu;

/// Per-repo location of the flake-rate baseline. Lives in repo root so
/// PR diffs surface baseline changes alongside the changes that
/// motivated them.
const BASELINE_PATH: &str = ".itest-baseline.toml";

/// Per-checkout integration-test lock. Lives at the repo root (with a
/// `.itest.lock` entry in `.gitignore`) so it's easy to find and
/// inspect — `cat .itest.lock` shows the PID of the current holder.
const LOCK_PATH: &str = ".itest.lock";

/// Per-checkout root for per-run history directories (tier 2 NDJSON +
/// tier 3 log copies). Each itest run creates a timestamped
/// subdirectory under here. Gitignored.
const HISTORY_ROOT: &str = ".itest-runs";

/// Default OTLP receiver targeted by `--push-otlp` (no value) and the
/// end-of-run auto-push. Matches `stack/docker-compose.yml`'s
/// Prometheus container started with `--web.enable-otlp-receiver`.
const DEFAULT_OTLP_ENDPOINT: &str = "http://127.0.0.1:9090/api/v1/otlp";

/// Set by the SIGINT handler. First Ctrl-C trips this to `true`; the
/// runner sees it at the next iteration boundary and writes a
/// partial baseline. Second Ctrl-C in the handler exits the process
/// directly.
static INTERRUPT: AtomicBool = AtomicBool::new(false);

fn install_ctrlc_handler() {
    let _ = ctrlc::set_handler(|| {
        // swap returns the previous value. If it was already true,
        // this is the second Ctrl-C → force-quit.
        if INTERRUPT.swap(true, Ordering::SeqCst) {
            eprintln!("\n(second Ctrl-C — force-quit; pending baseline NOT written)");
            std::process::exit(130);
        } else {
            eprintln!(
                "\n(Ctrl-C — finishing current iteration; \
                 partial baseline will be written if --update-baseline. \
                 Press Ctrl-C again to force-quit.)"
            );
        }
    });
}

mod harness;
mod matchers;
mod scenarios;

/// Use the harness's scenario type — same shape (name + fn pointer);
/// the runner loop lives there too. The list below is the `SnitchOS`
/// catalog.
use itest_harness::Scenario;

// CPU-profile classification per plans/itest-parallel-scenarios.md
// step 5. `cpu_bound` scenarios run real guest work between heartbeats
// (allocator pressure, storm workloads, context-switch loops) and get
// a serial pass in the parallel runner so they don't contend with each
// other. `new` scenarios are wfi-bounded and fan out across the worker
// pool. Initial classification is conservative — refine with `top`/`htop`
// observation.
const SCENARIOS: &[Scenario] = &[
    Scenario::new       ("boot-reaches-heartbeat",      scenarios::boot_reaches_heartbeat),
    Scenario::new       ("heartbeat-cadence",           scenarios::heartbeat_cadence),
    Scenario::new       ("pre-init-order",              scenarios::pre_init_order),
    Scenario::new       ("kernel-runs-at-higher-half",  scenarios::kernel_runs_at_higher_half),
    Scenario::new       ("frame-allocator-metrics",     scenarios::frame_allocator_metrics),
    Scenario::new       ("frame-allocator-oom",         scenarios::frame_allocator_oom),
    Scenario::new       ("kernel-heap-metrics",         scenarios::kernel_heap_metrics),
    Scenario::new       ("sched-context-switch-smoke",  scenarios::sched_context_switch_smoke),
    Scenario::new       ("sched-spawn-registers-thread", scenarios::sched_spawn_registers_thread),
    Scenario::cpu_bound ("sched-yield-round-trips",     scenarios::sched_yield_round_trips),
    Scenario::new       ("sched-spans-carry-task-id",   scenarios::sched_spans_carry_task_id),
    Scenario::new       ("sched-context-switches-on-wire", scenarios::sched_context_switches_on_wire),
    Scenario::new       ("sched-span-survives-yield",   scenarios::sched_span_survives_yield),
    Scenario::cpu_bound ("heap-oom",                    scenarios::heap_oom),
    Scenario::cpu_bound ("workload-cooperative-baseline", scenarios::workload_cooperative_baseline),
    Scenario::cpu_bound ("smp-producer-consumer-correctness", scenarios::smp_producer_consumer_correctness),
    Scenario::new       ("ipi-self-wakeup",             scenarios::ipi_self_wakeup),
    Scenario::new       ("smp-secondary-hart-boots",    scenarios::smp_secondary_hart_boots),
    Scenario::new       ("smp-spawn-on-hart-1-runs",    scenarios::smp_spawn_on_hart_1_runs),
    Scenario::new       ("smp-spans-carry-hart-id",     scenarios::smp_spans_carry_hart_id),
    Scenario::new       ("smp-ipi-wakes-idle-hart",     scenarios::smp_ipi_wakes_idle_hart),
    Scenario::cpu_bound ("spawn-storm",         scenarios::spawn_storm),
    Scenario::cpu_bound ("ipi-pong",            scenarios::ipi_pong),
    Scenario::cpu_bound ("shootdown-storm",     scenarios::shootdown_storm),
    Scenario::cpu_bound ("smp-tlb-shootdown-visible", scenarios::smp_tlb_shootdown_visible),
    Scenario::cpu_bound ("smp-ping-pong-cadence",     scenarios::smp_ping_pong_cadence),
    Scenario::new       ("sched-task-exits-cleanly",    scenarios::sched_task_exits_cleanly),
    Scenario::cpu_bound ("mutex-storm",         scenarios::mutex_storm),
    Scenario::cpu_bound ("virtio-storm",        scenarios::virtio_storm),
];

/// Entry point from `main`. `Some(name)` runs one scenario;
/// `None` runs them all. `repeat` controls how many full passes
/// to perform — useful for surfacing flaky scenarios.
/// `keep_existing_qemus` disables the pre-run cleanup of stale
/// `qemu-system-riscv64` processes (default: cleanup runs).
/// `update_baseline` writes the current run's per-scenario results
/// back to `.itest-baseline.toml` (pushing the previous `current`
/// into `history`) after the run completes.
/// Set the process-wide failure-capture transcript depth. Call once at
/// startup, before `run`. Delegates to the harness, which reads it at
/// every `Harness::spawn`.
pub fn set_capture_level(level: itest_harness::CaptureLevel) {
    harness::set_capture_level(level);
}

#[allow(clippy::too_many_arguments, reason = "1:1 with the CLI flags; refactor when more land")]
pub fn run(
    name: Option<&str>,
    repeat: u32,
    force: bool,
    update_baseline: bool,
    fail_fast: Option<u32>,
    auto_push: bool,
    jobs: u32,
    cpu_jobs: u32,
    profile_filter: Option<CpuProfile>,
    skip: &[String],
) -> ExitCode {
    if !qemu_available() {
        eprintln!("xtask test: qemu-system-riscv64 not on PATH — skipping");
        return ExitCode::SUCCESS;
    }

    // Acquire the integration-test lock. `--force` bypasses; otherwise
    // any contender (concurrent invocation from another terminal, agent,
    // or CI job on the same checkout) gets rejected here with the
    // holder's PID. The guard is held until `run` returns.
    let _lock_guard = if force {
        None
    } else {
        match ItestLock::acquire(Path::new(LOCK_PATH)) {
            Ok(guard) => Some(guard),
            Err(LockError::AlreadyHeld { pid }) => {
                eprintln!("error: {}", LockError::AlreadyHeld { pid });
                eprintln!("       Pass --force if you know the lock is stale.");
                return ExitCode::from(2);
            }
            Err(LockError::Io(e)) => {
                eprintln!("error: failed to acquire itest lock at {LOCK_PATH}: {e}");
                return ExitCode::from(2);
            }
        }
    };

    // Warn (but don't kill) about pre-existing qemus. The lock above
    // already prevents itest-vs-itest races; the remaining concern is a
    // user's `xtask boot` / `xtask debug` / manual QEMU running in
    // parallel. We surface the situation rather than silently murder it.
    let stale = detect_stale_qemus();
    if !stale.is_empty() {
        eprintln!(
            "warning: {} stale qemu-system-riscv64 process(es) detected (pid {}).",
            stale.len(),
            stale.iter().map(u32::to_string).collect::<Vec<_>>().join(", ")
        );
        eprintln!(
            "         Probably from `xtask boot`/`xtask debug` or a manual invocation."
        );
        eprintln!(
            "         Cross-test interference is possible. Kill them manually if needed."
        );
    }

    let to_run: Vec<&Scenario> = match name {
        // One name, or a comma-separated list (`itest a,b,c`).
        // Whitespace around each name is trimmed; any unknown name is a
        // hard error — a typo shouldn't silently run a subset.
        Some(n) => {
            let mut selected = Vec::new();
            for part in n.split(',').map(str::trim).filter(|p| !p.is_empty()) {
                match SCENARIOS.iter().find(|s| s.name == part) {
                    Some(s) => selected.push(s),
                    None => {
                        eprintln!("unknown scenario: {part}");
                        eprintln!("known: {}", SCENARIOS.iter().map(|s| s.name).collect::<Vec<_>>().join(", "));
                        return ExitCode::from(2);
                    }
                }
            }
            if selected.is_empty() {
                eprintln!("no scenarios selected (empty list?)");
                return ExitCode::from(2);
            }
            selected
        }
        None => SCENARIOS.iter().collect(),
    };
    let to_run: Vec<&Scenario> = match profile_filter {
        Some(p) => {
            let label = match p {
                CpuProfile::Wfi => "wfi",
                CpuProfile::Cpu => "cpu",
            };
            let filtered: Vec<&Scenario> =
                to_run.into_iter().filter(|s| s.cpu_profile == p).collect();
            if filtered.is_empty() {
                eprintln!("--profile {label}: no scenarios match this filter");
                return ExitCode::from(2);
            }
            eprintln!("--profile {label}: {} scenarios selected", filtered.len());
            filtered
        }
        None => to_run,
    };
    let to_run: Vec<&Scenario> = if skip.is_empty() {
        to_run
    } else {
        // Warn on unknown skip names — usually a typo, and silently
        // skipping nothing would hide it.
        for s in skip {
            if !SCENARIOS.iter().any(|sc| sc.name == s.as_str()) {
                eprintln!("warning: --skip {s:?}: no such scenario (ignored)");
            }
        }
        let before = to_run.len();
        let filtered: Vec<&Scenario> = to_run
            .into_iter()
            .filter(|sc| !skip.iter().any(|s| s == sc.name))
            .collect();
        eprintln!("--skip: excluded {} scenario(s)", before - filtered.len());
        if filtered.is_empty() {
            eprintln!("--skip: all selected scenarios were excluded; nothing to run");
            return ExitCode::from(2);
        }
        filtered
    };

    // Hook closures. None of these escape the call — the lifetime
    // parameter on RunnerConfig keeps them bounded to this scope.
    // One build for the whole suite: the `itest-workloads` kernel.
    // With no `workload=` bootarg it runs the exact default demo
    // (additive guarantee), so default-demo scenarios use it as-is;
    // workload scenarios select via QEMU `-append`. No per-scenario
    // rebuilds. See `docs/runtime-workload-selection-design.md`.
    let build = || {
        qemu::build_kernel(&["itest-workloads"])
            .map(|_| ())
            .map_err(|e| e.to_string())
    };
    let log_path_for = |_scenario_name: &str| harness::take_last_log_path();
    let max_wait_for = harness::take_last_max_wait;
    let capture_for = harness::take_last_failure_capture;
    let commit_for = current_commit_short;

    // Install the SIGINT handler before constructing config — the
    // INTERRUPT flag is what the runner reads at iteration boundaries.
    install_ctrlc_handler();

    let config = RunnerConfig {
        one_shot_build: Some(&build),
        log_path_for: Some(&log_path_for),
        max_wait_for: Some(&max_wait_for),
        capture_for: Some(&capture_for),
        current_commit: Some(&commit_for),
        baseline_file: Some(PathBuf::from(BASELINE_PATH)),
        fail_fast,
        pending_baseline: Some(PathBuf::from(format!("{BASELINE_PATH}.pending"))),
        interrupt: Some(&INTERRUPT),
        history_root: Some(PathBuf::from(HISTORY_ROOT)),
        jobs,
        cpu_jobs,
        invocation: Some(std::env::args().collect::<Vec<_>>().join(" ")),
    };

    let outcome = itest_harness::run(&to_run, repeat, update_baseline, &config);

    if auto_push {
        try_auto_push();
    }

    outcome
}

/// Probe the bundled stack's OTLP receiver and push the canonical
/// baseline if it answers. Warn (don't be silent) when it doesn't —
/// the user opted in to live metrics by enabling auto-push (it's on
/// by default), so silent skipping would hide the misconfiguration.
///
/// Bounded by a short connect timeout so a missing stack costs ~1s
/// at the end of a run, not ureq's default ~75s.
fn try_auto_push() {
    let endpoint = DEFAULT_OTLP_ENDPOINT;
    let baseline_path = Path::new(BASELINE_PATH);
    let file = if baseline_path.exists() {
        match BaselineFile::load_path(baseline_path) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("auto-push: skipped — failed to parse {BASELINE_PATH}: {e}");
                return;
            }
        }
    } else {
        // No baseline yet (e.g. first run on a fresh checkout).
        // Nothing to push; silent skip is correct here.
        return;
    };
    let now_ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos().min(u128::from(u64::MAX)) as u64);
    let connect_timeout = Some(std::time::Duration::from_secs(1));
    let read_timeout = Some(std::time::Duration::from_secs(3));
    match push_otlp_with_timeout(endpoint, &file, now_ns, connect_timeout, read_timeout) {
        Ok(status) if (200..300).contains(&status) => {
            eprintln!(
                "auto-push: pushed {} scenarios to {endpoint} (HTTP {status})",
                file.scenarios.len()
            );
        }
        Ok(status) => {
            eprintln!(
                "auto-push: OTLP receiver at {endpoint} returned HTTP {status}.\n\
                 Confirm the stack is healthy, or pass --no-auto-push to silence."
            );
        }
        Err(e) => {
            eprintln!(
                "auto-push: OTLP receiver at {endpoint} not reachable ({e}).\n\
                 Run `cargo xtask stack up`, or pass --no-auto-push to silence."
            );
        }
    }
}

/// Promote `.itest-baseline.toml.pending` into the canonical baseline
/// file. Wraps `BaselineFile::promote_pending` with user-facing
/// messaging and `--baseline-show`-friendly exit codes.
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
/// `endpoint`. Endpoint should be the OTLP receiver root, e.g.
/// `http://localhost:9090/api/v1/otlp` for Prometheus with
/// `--web.enable-otlp-receiver`, or `http://localhost:4318` for the
/// `OTel` collector default. `/v1/metrics` is appended automatically.
pub fn push_otlp_metrics(endpoint: &str) -> ExitCode {
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
        eprintln!("no baseline file at {BASELINE_PATH}; nothing to push");
        return ExitCode::SUCCESS;
    };
    // Wall-clock nanoseconds since the epoch. Same timestamp on every
    // data point in this batch — they're all observations of the
    // same baseline at the same instant.
    let now_ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos().min(u128::from(u64::MAX)) as u64);
    match push_otlp(endpoint, &file, now_ns) {
        Ok(status) if (200..300).contains(&status) => {
            eprintln!(
                "Pushed {} scenarios to {} (HTTP {status})",
                file.scenarios.len(),
                endpoint
            );
            ExitCode::SUCCESS
        }
        Ok(status) => {
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
/// exists — caller should `--discard-pending` or `--promote-pending`
/// first, then re-run recovery.
pub fn recover_pending(run_dir: &Path) -> ExitCode {
    let canonical = Path::new(BASELINE_PATH);
    let pending = BaselineFile::pending_path_for(canonical);
    if pending.exists() {
        eprintln!(
            "refusing to overwrite existing pending baseline at {}.\n\
             Promote (--promote-pending) or discard (--discard-pending) it first.",
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
         {} scenarios reconstructed. Inspect with --baseline-show, then\n\
         --promote-pending or --discard-pending.",
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
/// `--promote-pending` or `--discard-pending`. When `pending` is
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
            "NOTE: pending baseline present at {} — inspect with `--baseline-show --pending`,\n\
             then promote (`--promote-pending`) or discard (`--discard-pending`).\n",
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

/// Returns the short commit hash for HEAD via `git rev-parse`. None on
/// any failure (not in a git checkout, git missing, etc.). The baseline
/// file falls back to "unknown" in that case.
fn current_commit_short() -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?;
    let trimmed = s.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Workspace crates that have host-runnable `cargo test` suites.
/// The kernel itself is `no_std`/`no_main` and won't build for the
/// host — testable logic lives in `kernel-core`. Each entry is the
/// crate name plus any extra args (features) the suite needs.
const UNIT_TEST_CRATES: &[(&str, &[&str])] = &[
    ("kernel-core", &[]),
    ("protocol", &["--features", "std"]),
    ("collector", &[]),
    ("itest-harness", &[]),
];

/// Run every workspace crate's host unit tests, in order. Returns
/// `SUCCESS` only if all crates pass. Bails out on first failure
/// (no point continuing if `kernel-core` is broken).
pub fn run_unit_tests() -> ExitCode {
    eprintln!("=== unit tests ===");
    for (crate_name, extra_args) in UNIT_TEST_CRATES {
        eprint!("  {crate_name} ... ");
        let status = std::process::Command::new("cargo")
            .args(["test", "-p", crate_name, "--quiet"])
            .args(*extra_args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .output();
        match status {
            Ok(out) if out.status.success() => eprintln!("ok"),
            Ok(out) => {
                eprintln!("FAILED");
                // Surface the actual test failure output so the user
                // doesn't have to re-run with --verbose.
                let stderr = String::from_utf8_lossy(&out.stderr);
                for line in stderr.lines() {
                    eprintln!("    {line}");
                }
                return ExitCode::from(1);
            }
            Err(e) => {
                eprintln!("FAILED to invoke cargo: {e}");
                return ExitCode::from(1);
            }
        }
    }
    ExitCode::SUCCESS
}

fn qemu_available() -> bool {
    std::process::Command::new("qemu-system-riscv64")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Wipe out any `qemu-system-riscv64` processes already on the host.
/// Run before the suite by default — a stale QEMU from `cargo xtask
/// boot`, a debug session, or a previous interrupted suite would
/// compete for host CPU and cause spurious flakes. Bypassed with
/// `--keep-existing-qemus`.
/// Return the PIDs of currently-running `qemu-system-riscv64`
/// processes. The integration-test lock prevents itest-vs-itest races
/// directly; this detection covers the remaining case of `xtask boot`,
/// `xtask debug`, or a manually-launched QEMU sharing the machine.
fn detect_stale_qemus() -> Vec<u32> {
    std::process::Command::new("pgrep")
        .arg("qemu-system-riscv64")
        .output()
        .ok()
        .map(|o| {
            std::str::from_utf8(&o.stdout)
                .map(|s| s.lines().filter_map(|l| l.trim().parse().ok()).collect())
                .unwrap_or_default()
        })
        .unwrap_or_default()
}
