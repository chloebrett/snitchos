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
    Scenario { name: "boot-reaches-heartbeat", run: scenarios::boot_reaches_heartbeat },
    Scenario { name: "heartbeat-cadence",      run: scenarios::heartbeat_cadence },
    Scenario { name: "pre-init-order",         run: scenarios::pre_init_order },
    Scenario { name: "mmu-enabled",            run: scenarios::mmu_enabled },
];

/// Entry point from `main`. `Some(name)` runs one scenario;
/// `None` runs them all.
pub fn run(name: Option<&str>) -> ExitCode {
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

    let mut failed = 0;
    for s in &to_run {
        eprint!("test {} ... ", s.name);
        match (s.run)() {
            Ok(()) => eprintln!("ok"),
            Err(e) => {
                eprintln!("FAILED");
                eprintln!("  {e}");
                failed += 1;
            }
        }
    }

    let total = to_run.len();
    eprintln!("\n{} passed, {} failed", total - failed, failed);
    if failed == 0 { ExitCode::SUCCESS } else { ExitCode::from(1) }
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
