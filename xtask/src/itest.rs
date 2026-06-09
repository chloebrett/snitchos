//! Kernel integration tests. Each scenario boots the kernel in QEMU,
//! reads frames off the virtio-console socket, and asserts on the
//! decoded `Frame` sequence. See `plans/kernel-integration-tests.md`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};

use itest_harness::{CpuProfile, ItestLock, LockError, RunnerConfig};

use crate::qemu;

/// Per-repo location of the flake-rate baseline. Lives in repo root so
/// PR diffs surface baseline changes alongside the changes that
/// motivated them.
pub(crate) const BASELINE_PATH: &str = ".itest-baseline.toml";

/// Per-checkout integration-test lock. Lives at the repo root (with a
/// `.itest.lock` entry in `.gitignore`) so it's easy to find and
/// inspect — `cat .itest.lock` shows the PID of the current holder.
const LOCK_PATH: &str = ".itest.lock";

/// Per-checkout root for per-run history directories (tier 2 NDJSON +
/// tier 3 log copies). Each itest run creates a timestamped
/// subdirectory under here. Gitignored.
pub(crate) const HISTORY_ROOT: &str = ".itest-runs";

/// Default OTLP receiver targeted by `baseline push` (no value) and the
/// end-of-run auto-push. Matches `stack/docker-compose.yml`'s
/// Prometheus container started with `--web.enable-otlp-receiver`.
pub(crate) const DEFAULT_OTLP_ENDPOINT: &str = "http://127.0.0.1:9090/api/v1/otlp";

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

pub(crate) mod baseline;
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
                let Some(s) = SCENARIOS.iter().find(|s| s.name == part) else {
                    eprintln!("unknown scenario: {part}");
                    eprintln!("known: {}", SCENARIOS.iter().map(|s| s.name).collect::<Vec<_>>().join(", "));
                    return ExitCode::from(2);
                };
                selected.push(s);
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
    let timeouts = Some((
        std::time::Duration::from_secs(1),
        std::time::Duration::from_secs(3),
    ));
    match baseline::load_and_push(endpoint, timeouts) {
        // No baseline yet (e.g. first run on a fresh checkout) — silent skip.
        Ok(None) => {}
        Ok(Some((status, scenarios))) if (200..300).contains(&status) => {
            eprintln!("auto-push: pushed {scenarios} scenarios to {endpoint} (HTTP {status})");
        }
        Ok(Some((status, _))) => {
            eprintln!(
                "auto-push: OTLP receiver at {endpoint} returned HTTP {status}.\n\
                 Confirm the stack is healthy, or pass --no-auto-push to silence."
            );
        }
        Err(e) => {
            eprintln!(
                "auto-push: skipped ({e}).\n\
                 Run `cargo xtask stack up`, or pass --no-auto-push to silence."
            );
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
