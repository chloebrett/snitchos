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
    /// Clippy the whole workspace correctly: host crates for the host,
    /// the kernel for its bare-metal riscv target (which plain
    /// `cargo clippy --workspace` can't do — it builds the kernel for the
    /// host, where it won't link). Trailing args are forwarded to both
    /// invocations, e.g. `cargo xtask clippy -- --fix --allow-dirty` or
    /// `cargo xtask clippy -- -- -D warnings`.
    Clippy {
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
    /// Use `--repeat N` to run the suite N times back-to-back; an
    /// aggregate flake report lists scenarios that failed in at
    /// least one run.
    ///
    /// By default, any `qemu-system-riscv64` processes already
    /// running on the host are killed before the suite starts — a
    /// stale QEMU from a prior `cargo xtask boot` or debug session
    /// would otherwise compete for host CPU and cause flakes. Use
    /// `--keep-existing-qemus` to disable this if you genuinely want
    /// concurrent QEMUs.
    Test {
        /// Optional scenario name to run. Omit to run all.
        scenario: Option<String>,
        /// Number of times to repeat the run. Useful for flake
        /// detection. Default 1.
        #[arg(long, default_value_t = 1)]
        repeat: u32,
        /// Skip the pre-run cleanup of stale `qemu-system-riscv64`
        /// processes. Off by default; use when you want to leave a
        /// concurrent debug QEMU alive (rare).
        #[arg(long, default_value_t = false)]
        keep_existing_qemus: bool,
    },
    /// Count lines of code across the workspace, split by crate and
    /// by production vs test lines.
    Loc,
    /// Build the kernel and run it under QEMU with a GDB stub (`-s -S`).
    /// QEMU halts at start and listens on localhost:1234; attach with
    /// lldb or riscv64-unknown-elf-gdb from another terminal. Prints
    /// ready-to-copy attach commands.
    ///
    /// Use `--features <feat>` to build feature-flagged kernels (e.g.
    /// `--features heap-oom` to debug the heap-oom regression).
    Debug {
        /// Cargo features to enable on the kernel build, comma-separated.
        #[arg(long, default_value = "")]
        features: String,
    },
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
        Cmd::Clippy { args } => run_clippy(&args),
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
        Cmd::Test { scenario, repeat, keep_existing_qemus } => {
            itest::run(scenario.as_deref(), repeat, keep_existing_qemus)
        }
        Cmd::Loc => loc::run(),
        Cmd::Debug { features } => debug(&features),
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

fn debug(features: &str) -> ExitCode {
    // Build with whatever features the caller requested.
    let features_vec: Vec<&str> = if features.is_empty() {
        Vec::new()
    } else {
        features.split(',').collect()
    };
    let status = qemu::build_kernel(&features_vec)
        .expect("failed to invoke cargo");
    if !status.success() {
        return ExitCode::from(1);
    }

    // Clean up any stale telemetry socket from a previous run.
    let _ = std::fs::remove_file(TELEMETRY_SOCKET);

    // `wait=off` so the chardev doesn't block — we want QEMU to halt
    // at start (via -S) waiting for the debugger, not waiting for a
    // telemetry client. Telemetry is irrelevant in debug runs.
    let chardev_arg =
        format!("socket,path={TELEMETRY_SOCKET},server=on,wait=off,id=telemetry");

    eprintln!();
    eprintln!("QEMU starting paused (halted at entry).");
    eprintln!("In another terminal, attach a debugger:");
    eprintln!();
    eprintln!("  # lldb (Apple-shipped on macOS):");
    eprintln!("  lldb target/{}/debug/kernel", qemu::KERNEL_TARGET);
    eprintln!("  (lldb) gdb-remote localhost:1234");
    eprintln!("  (lldb) breakpoint set --name kmain");
    eprintln!("  (lldb) breakpoint set --name _start");
    eprintln!("  (lldb) continue");
    eprintln!();
    eprintln!("  # GDB (from `brew install riscv64-elf-gdb`):");
    eprintln!("  riscv64-elf-gdb target/{}/debug/kernel", qemu::KERNEL_TARGET);
    eprintln!("  (gdb) target remote :1234");
    eprintln!("  (gdb) break kmain");
    eprintln!("  (gdb) break _start");
    eprintln!("  (gdb) continue");
    eprintln!();
    eprintln!("Useful: `si` (step instruction), `info registers`,");
    eprintln!("`disassemble`, `x/16i $pc` (next 16 instructions).");
    eprintln!();
    eprintln!("Pre-MMU debugging gotcha: `break kmain` resolves to the");
    eprintln!("higher-half VA (0xffffffff8020....), which won't fire while");
    eprintln!("the MMU is off and the kernel runs at PA 0x8020.... For");
    eprintln!("pre-trampoline breakpoints, use the physical address:");
    eprintln!("`break *0x80204724` (= linker VA minus KERNEL_OFFSET).");
    eprintln!("After the trampoline, symbol breakpoints work normally.");
    eprintln!();

    let status = qemu::base_command(&chardev_arg)
        // -s = listen on localhost:1234 for GDB.
        // -S = halt CPU at startup; wait for the debugger to `continue`.
        .args(["-s", "-S"])
        .status()
        .expect("failed to invoke qemu-system-riscv64");
    if status.success() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
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

fn run_clippy(extra_args: &[String]) -> ExitCode {
    // Host-buildable crates: lint everything including tests.
    let host = Command::new("cargo")
        .args([
            "clippy",
            "-p", "kernel-core",
            "-p", "protocol",
            "-p", "collector",
            "-p", "xtask",
            "--all-targets",
        ])
        .args(extra_args)
        .status()
        .expect("failed to invoke cargo clippy");

    // The kernel only compiles for bare-metal riscv. No `--all-targets`:
    // it has no host-buildable test target.
    let kernel = Command::new("cargo")
        .args(["clippy", "-p", "kernel", "--target", qemu::KERNEL_TARGET])
        .args(extra_args)
        .status()
        .expect("failed to invoke cargo clippy");

    if host.success() && kernel.success() {
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
