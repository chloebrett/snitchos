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
pub fn run(name: Option<&str>, repeat: u32) -> ExitCode {
    use std::collections::BTreeMap;

    if !qemu_available() {
        eprintln!("xtask test: qemu-system-riscv64 not on PATH — skipping");
        return ExitCode::SUCCESS;
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
                Ok(()) => eprintln!("ok{timing_str}"),
                Err(e) => {
                    eprintln!("FAILED{timing_str}");
                    eprintln!("  {e}");
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

fn qemu_available() -> bool {
    std::process::Command::new("qemu-system-riscv64")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}
