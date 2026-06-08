//! Kernel integration tests. Each scenario boots the kernel in QEMU,
//! reads frames off the virtio-console socket, and asserts on the
//! decoded `Frame` sequence. See `plans/kernel-integration-tests.md`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use itest_harness::{BaselineFile, ItestLock, LockError, RunnerConfig};

use crate::qemu;

/// Per-repo location of the flake-rate baseline. Lives in repo root so
/// PR diffs surface baseline changes alongside the changes that
/// motivated them.
const BASELINE_PATH: &str = ".itest-baseline.toml";

/// Per-checkout integration-test lock. Lives at the repo root (with a
/// `.itest.lock` entry in `.gitignore`) so it's easy to find and
/// inspect — `cat .itest.lock` shows the PID of the current holder.
const LOCK_PATH: &str = ".itest.lock";

mod harness;
mod matchers;
mod scenarios;

/// Use the harness's scenario type — same shape (name + fn pointer);
/// the runner loop lives there too. The list below is the SnitchOS
/// catalog.
use itest_harness::Scenario;

const SCENARIOS: &[Scenario] = &[
    Scenario { name: "boot-reaches-heartbeat",     run: scenarios::boot_reaches_heartbeat },
    Scenario { name: "heartbeat-cadence",          run: scenarios::heartbeat_cadence },
    Scenario { name: "pre-init-order",             run: scenarios::pre_init_order },
    Scenario { name: "kernel-runs-at-higher-half", run: scenarios::kernel_runs_at_higher_half },
    Scenario { name: "frame-allocator-metrics",    run: scenarios::frame_allocator_metrics },
    Scenario { name: "frame-allocator-oom",        run: scenarios::frame_allocator_oom },
    Scenario { name: "kernel-heap-metrics",        run: scenarios::kernel_heap_metrics },
    Scenario { name: "sched-context-switch-smoke", run: scenarios::sched_context_switch_smoke },
    Scenario { name: "sched-spawn-registers-thread", run: scenarios::sched_spawn_registers_thread },
    Scenario { name: "sched-yield-round-trips",      run: scenarios::sched_yield_round_trips },
    Scenario { name: "sched-spans-carry-task-id",    run: scenarios::sched_spans_carry_task_id },
    Scenario { name: "sched-context-switches-on-wire", run: scenarios::sched_context_switches_on_wire },
    Scenario { name: "sched-span-survives-yield",    run: scenarios::sched_span_survives_yield },
    Scenario { name: "heap-oom",                   run: scenarios::heap_oom },
    Scenario { name: "workload-cooperative-baseline", run: scenarios::workload_cooperative_baseline },
    Scenario { name: "ipi-self-wakeup",            run: scenarios::ipi_self_wakeup },
    Scenario { name: "smp-secondary-hart-boots",   run: scenarios::smp_secondary_hart_boots },
    Scenario { name: "smp-spawn-on-hart-1-runs",   run: scenarios::smp_spawn_on_hart_1_runs },
    Scenario { name: "deflake-spawn-storm",        run: scenarios::deflake_spawn_storm },
    Scenario { name: "deflake-ipi-pong",           run: scenarios::deflake_ipi_pong },
    Scenario { name: "deflake-shootdown-storm",    run: scenarios::deflake_shootdown_storm },
    Scenario { name: "sched-task-exits-cleanly",   run: scenarios::sched_task_exits_cleanly },
];

/// Entry point from `main`. `Some(name)` runs one scenario;
/// `None` runs them all. `repeat` controls how many full passes
/// to perform — useful for surfacing flaky scenarios.
/// `keep_existing_qemus` disables the pre-run cleanup of stale
/// `qemu-system-riscv64` processes (default: cleanup runs).
/// `update_baseline` writes the current run's per-scenario results
/// back to `.itest-baseline.toml` (pushing the previous `current`
/// into `history`) after the run completes.
pub fn run(
    name: Option<&str>,
    repeat: u32,
    force: bool,
    update_baseline: bool,
    fail_fast: Option<u32>,
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
        Some(n) => match SCENARIOS.iter().find(|s| s.name == n) {
            Some(s) => vec![s],
            None => {
                eprintln!("unknown scenario: {n}");
                eprintln!("known: {}", SCENARIOS.iter().map(|s| s.name).collect::<Vec<_>>().join(", "));
                return ExitCode::from(2);
            }
        },
        None => SCENARIOS.iter().collect(),
    };

    // Hook closures. None of these escape the call — the lifetime
    // parameter on RunnerConfig keeps them bounded to this scope.
    let build = || {
        qemu::build_kernel(&[])
            .map(|_| ())
            .map_err(|e| e.to_string())
    };
    let log_path_for = |_scenario_name: &str| harness::take_last_log_path();
    let max_wait_for = harness::take_last_max_wait;
    let commit_for = current_commit_short;

    let config = RunnerConfig {
        // kill_stale is intentionally None — the lock + warning combo
        // above replaces the previous murder-everything approach.
        kill_stale: None,
        one_shot_build: Some(&build),
        log_path_for: Some(&log_path_for),
        max_wait_for: Some(&max_wait_for),
        current_commit: Some(&commit_for),
        baseline_file: Some(PathBuf::from(BASELINE_PATH)),
        fail_fast,
    };

    itest_harness::run(&to_run, repeat, update_baseline, &config)
}

/// Load `.itest-baseline.toml` and print its rendered summary. Exits
/// with `0` on success (including "file doesn't exist" — that's a
/// valid initial state). Returns `1` only on parse error.
pub fn show_baseline() -> ExitCode {
    let path = Path::new(BASELINE_PATH);
    if !path.exists() {
        eprintln!("no baseline file at {BASELINE_PATH}");
        return ExitCode::SUCCESS;
    }
    match BaselineFile::load_path(path) {
        Ok(file) => {
            eprintln!("=== {BASELINE_PATH} ===\n");
            eprint!("{}", file.render_summary());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("failed to parse {BASELINE_PATH}: {e}");
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
        .map(|s| s.success())
        .unwrap_or(false)
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
