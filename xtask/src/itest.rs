//! Kernel integration tests. Each scenario boots the kernel in QEMU,
//! reads frames off the virtio-console socket, and asserts on the
//! decoded `Frame` sequence. See `plans/kernel-integration-tests.md`.

use std::process::ExitCode;

mod harness;
mod matchers;
mod scenarios;

/// One scenario registered with the runner. Name is what the user
/// types on the CLI; `run` returns `Ok(())` or a human-readable
/// failure reason.
pub(crate) struct Scenario {
    pub name: &'static str,
    pub run: fn() -> Result<(), String>,
}

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
];

/// Entry point from `main`. `Some(name)` runs one scenario;
/// `None` runs them all. `repeat` controls how many full passes
/// to perform — useful for surfacing flaky scenarios.
/// `keep_existing_qemus` disables the pre-run cleanup of stale
/// `qemu-system-riscv64` processes (default: cleanup runs).
pub fn run(name: Option<&str>, repeat: u32, keep_existing_qemus: bool) -> ExitCode {
    use std::collections::BTreeMap;

    if !qemu_available() {
        eprintln!("xtask test: qemu-system-riscv64 not on PATH — skipping");
        return ExitCode::SUCCESS;
    }

    if !keep_existing_qemus {
        kill_stale_qemus();
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

    let runs = repeat.max(1);
    // Per-scenario fail counter across all runs. BTreeMap keeps the
    // aggregate report in scenario-registration order via the name key.
    let mut fail_count: BTreeMap<&str, u32> = BTreeMap::new();
    // Per-run pass/fail totals — printed in the aggregate at the end.
    let mut run_totals: Vec<(usize, usize)> = Vec::with_capacity(runs as usize);

    for run_idx in 0..runs {
        if runs > 1 {
            eprintln!("\n=== run {}/{} ===", run_idx + 1, runs);
        }
        let mut failed_this_run = 0;
        for s in &to_run {
            eprint!("test {} ... ", s.name);
            let outcome = (s.run)();
            // Drained after the scenario returns (Harness::Drop wrote it).
            // `None` if the scenario never built a Harness, which today
            // shouldn't happen but is harmless.
            let timing = harness::take_last_max_wait();
            let timing_str = timing
                .map(|(actual, budget)| {
                    format!(
                        " (max wait {:.1}s of {:.0}s budget)",
                        actual.as_secs_f64(),
                        budget.as_secs_f64()
                    )
                })
                .unwrap_or_default();
            match outcome {
                Ok(()) => {
                    eprintln!("ok{timing_str}");
                    // Drained but unused on success — keeps the
                    // thread-local empty for the next scenario.
                    let _ = harness::take_last_log_path();
                }
                Err(e) => {
                    eprintln!("FAILED{timing_str}");
                    eprintln!("  {e}");
                    if let Some(log_path) = harness::take_last_log_path() {
                        match std::fs::read_to_string(&log_path) {
                            Ok(contents) if !contents.trim().is_empty() => {
                                eprintln!("  --- QEMU log ({}) ---", log_path.display());
                                // Tail-style: last ~80 lines, enough
                                // to catch a panic + preceding boot
                                // markers without flooding the
                                // terminal.
                                let lines: Vec<&str> = contents.lines().collect();
                                let tail_start = lines.len().saturating_sub(80);
                                for line in &lines[tail_start..] {
                                    eprintln!("  | {line}");
                                }
                                eprintln!("  --- end QEMU log ---");
                            }
                            Ok(_) => {}
                            Err(e) => eprintln!("  (failed to read log {}: {e})", log_path.display()),
                        }
                    }
                    failed_this_run += 1;
                    *fail_count.entry(s.name).or_insert(0) += 1;
                }
            }
        }
        let total = to_run.len();
        eprintln!("\n{} passed, {} failed", total - failed_this_run, failed_this_run);
        run_totals.push((total - failed_this_run, failed_this_run));
    }

    // Single-run path: behaviour unchanged from before.
    if runs == 1 {
        return if run_totals[0].1 == 0 { ExitCode::SUCCESS } else { ExitCode::from(1) };
    }

    // Multi-run aggregate: per-run summary + flake table.
    eprintln!("\n=== aggregate over {runs} runs ===");
    for (i, (pass, fail)) in run_totals.iter().enumerate() {
        eprintln!("  run {}: {pass} passed, {fail} failed", i + 1);
    }
    let total_runs = runs;
    if fail_count.is_empty() {
        eprintln!("\nNo flakes — every scenario passed every run.");
        ExitCode::SUCCESS
    } else {
        eprintln!("\nFlaky scenarios (failed at least once):");
        for (name, count) in &fail_count {
            eprintln!("  {name}: {count}/{total_runs} runs failed");
        }
        ExitCode::from(1)
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
///
/// Uses `pkill -9` so we don't wait for SIGTERM handlers; if any
/// QEMU genuinely owns important state the user should pass the
/// flag. After killing, briefly polls until the process table is
/// clear (cap at 2s) so the suite doesn't immediately race the
/// scheduler reaping the corpses.
fn kill_stale_qemus() {
    use std::time::{Duration, Instant};

    let count_before = pgrep_count("qemu-system-riscv64");
    if count_before == 0 {
        return;
    }

    let _ = std::process::Command::new("pkill")
        .args(["-9", "qemu-system-riscv64"])
        .status();

    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if pgrep_count("qemu-system-riscv64") == 0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    let count_after = pgrep_count("qemu-system-riscv64");
    if count_after > 0 {
        eprintln!(
            "xtask test: warning — {count_after} qemu-system-riscv64 \
             process(es) still alive after pkill -9, suite may flake"
        );
    } else {
        eprintln!(
            "xtask test: killed {count_before} stale qemu-system-riscv64 \
             process(es) (use --keep-existing-qemus to skip)"
        );
    }
}

fn pgrep_count(pattern: &str) -> u32 {
    std::process::Command::new("pgrep")
        .arg(pattern)
        .output()
        .ok()
        .map(|o| {
            std::str::from_utf8(&o.stdout)
                .map(|s| s.lines().count() as u32)
                .unwrap_or(0)
        })
        .unwrap_or(0)
}
