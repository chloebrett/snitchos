use std::process::{Command, ExitCode};

use clap::{Parser, Subcommand};

mod itest;
mod loc;
mod qemu;

const COLLECTOR_BIN: &str = "target/debug/collector";
const TELEMETRY_SOCKET: &str = "/tmp/snitch-telemetry.sock";

/// Orchestration commands for the SnitchOS workspace.
#[derive(Parser)]
#[command(about, version)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Build the kernel ELF.
    Build,
    /// Build the kernel and run it in QEMU.
    Boot,
    /// Build and run the collector (telemetry consumer). Trailing args
    /// are forwarded to the collector, e.g.
    /// `cargo xtask collect -- --text --otlp http://localhost:4318`.
    Collect {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Build and run the collector in text-only mode (decoded frames
    /// printed to stdout). Shorthand for `collect -- --text`.
    Reader {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Run mutation testing against the host-testable crates (collector +
    /// protocol). Trailing args are forwarded to cargo-mutants, e.g.
    /// `cargo xtask mutants -- -j 4`.
    Mutants {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Bring the observability stack (Tempo + Prometheus + Grafana)
    /// up or down via docker compose.
    Stack {
        #[command(subcommand)]
        cmd: StackCmd,
    },
    /// Run kernel integration tests in QEMU. With no scenario name,
    /// runs every known scenario and reports a pass/fail summary.
    Test {
        /// Optional scenario name to run. Omit to run all.
        scenario: Option<String>,
    },
    /// Count lines of code across the workspace, split by crate and
    /// by production vs test lines.
    Loc,
}

#[derive(Subcommand)]
enum StackCmd {
    /// `docker compose up -d` the stack.
    Up,
    /// `docker compose down` the stack.
    Down,
    /// `docker compose logs -f` the stack.
    Logs,
}

fn main() -> ExitCode {
    match Cli::parse().cmd {
        Cmd::Build => build(),
        Cmd::Boot => boot(),
        Cmd::Mutants { args } => run_mutants(&args),
        Cmd::Collect { args } => run_collector(&args),
        Cmd::Reader { args } => {
            // Reader = text-only debug view; no docker dependency.
            let mut all = vec![
                "--text".to_string(),
                "--no-otlp".to_string(),
                "--no-loki".to_string(),
                "--no-prometheus".to_string(),
            ];
            all.extend(args);
            run_collector(&all)
        }
        Cmd::Stack { cmd } => stack(cmd),
        Cmd::Test { scenario } => itest::run(scenario.as_deref()),
        Cmd::Loc => loc::run(),
    }
}

fn stack(cmd: StackCmd) -> ExitCode {
    let subcommand: &[&str] = match cmd {
        StackCmd::Up => &["up", "-d"],
        StackCmd::Down => &["down"],
        StackCmd::Logs => &["logs", "-f"],
    };

    let status = Command::new("docker")
        .args(["compose", "-f", "stack/docker-compose.yml"])
        .args(subcommand)
        .status()
        .expect("failed to invoke docker compose");
    if status.success() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

fn build() -> ExitCode {
    let status = qemu::build_kernel(&[]).expect("failed to invoke cargo");
    if status.success() { ExitCode::SUCCESS } else { ExitCode::from(1) }
}

fn boot() -> ExitCode {
    if build() != ExitCode::SUCCESS {
        return ExitCode::from(1);
    }

    // Clean up any stale socket from a previous run so QEMU can bind.
    let _ = std::fs::remove_file(TELEMETRY_SOCKET);

    // wait=on blocks QEMU at startup until a telemetry client connects.
    // Run `cargo xtask collect` (or `cargo xtask reader`) in another
    // terminal to satisfy that wait.
    let chardev_arg =
        format!("socket,path={TELEMETRY_SOCKET},server=on,wait=on,id=telemetry");

    let status = qemu::base_command(&chardev_arg)
        .status()
        .expect("failed to invoke qemu-system-riscv64");
    if status.success() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

fn run_mutants(extra_args: &[String]) -> ExitCode {
    let status = Command::new("cargo")
        .args(["mutants", "-p", "collector", "-p", "protocol", "-p", "kernel-core", "--features", "protocol/std"])
        .args(extra_args)
        .status()
        .expect("failed to invoke cargo mutants");
    if status.success() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

fn run_collector(extra_args: &[String]) -> ExitCode {
    let build = Command::new("cargo")
        .args(["build", "-p", "collector"])
        .status()
        .expect("failed to invoke cargo");
    if !build.success() {
        return ExitCode::from(1);
    }

    let status = Command::new(COLLECTOR_BIN)
        .args(extra_args)
        .status()
        .expect("failed to invoke collector");
    if status.success() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}
