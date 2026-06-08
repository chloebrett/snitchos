use std::process::{Command, ExitCode};

use clap::{Parser, Subcommand, ValueEnum};

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
    /// Run all host-side unit tests across the workspace
    /// (`kernel-core`, `protocol --features std`, `collector`).
    /// Fast (~1s). Doesn't touch QEMU.
    Test,
    /// Run kernel integration tests in QEMU. By default, runs the
    /// workspace unit tests first and only proceeds to integration
    /// if they all pass (use `--skip-unit-tests` to bypass).
    ///
    /// With no scenario name, runs every known integration
    /// scenario and reports a pass/fail summary. Use `--repeat N`
    /// to run the suite N times back-to-back; an aggregate flake
    /// report lists scenarios that failed in at least one run.
    ///
    /// By default, any `qemu-system-riscv64` processes already
    /// running on the host are killed before the suite starts — a
    /// stale QEMU from a prior `cargo xtask boot` or debug session
    /// would otherwise compete for host CPU and cause flakes. Use
    /// `--keep-existing-qemus` to disable this if you genuinely
    /// want concurrent QEMUs.
    Itest {
        /// Optional scenario name to run. Omit to run all.
        scenario: Option<String>,
        /// Number of times to repeat the run. Useful for flake
        /// detection. Default 1.
        #[arg(long, default_value_t = 1)]
        repeat: u32,
        /// Ignore the integration-test lock at `target/.itest.lock`.
        /// Default off: only one itest run may proceed at a time;
        /// concurrent invocations (from another agent, terminal, or
        /// CI job on the same checkout) get rejected with the holder's
        /// PID. Pass `--force` if you know the lock is stale (rare —
        /// the OS releases on holder death, including Ctrl-C).
        #[arg(long, default_value_t = false)]
        force: bool,
        /// Skip running the workspace unit tests as a prerequisite.
        /// Off by default — unit tests run first; integration only
        /// proceeds if they pass.
        #[arg(long, default_value_t = false)]
        skip_unit_tests: bool,
        /// After the run, overwrite the `current` baseline entry for
        /// each scenario in `.itest-baseline.toml` with the current
        /// run's results. The previous `current` (if any) is pushed
        /// onto `history`. Use after an intentional rate change.
        #[arg(long, default_value_t = false)]
        update_baseline: bool,
        /// Abort the `--repeat` sweep once the cumulative failure count
        /// across all scenarios and runs reaches this value. Default
        /// off (the run goes to completion). Useful for "confirm
        /// flakiness fast" — with `--fail-fast 3`, a flaky kernel
        /// usually wraps within ~30 scenario-runs instead of the full
        /// `--repeat N`. Check fires at iteration boundaries.
        #[arg(long)]
        fail_fast: Option<u32>,
        /// Print the `.itest-baseline.toml` summary and exit without
        /// running anything. Useful for quickly inspecting recorded
        /// per-scenario rates. By default shows only the `current`
        /// entry per scenario; pass `--include-history` for the full
        /// chronological listing.
        #[arg(long, default_value_t = false)]
        baseline_show: bool,
        /// Include each scenario's prior `current` measurements
        /// (i.e. `history`) in `--baseline-show`. Ignored unless
        /// `--baseline-show` is also passed.
        #[arg(long, default_value_t = false)]
        include_history: bool,
        /// Restrict `--baseline-show` to scenarios with at least one
        /// failure recorded in `current`. Sorted descending by
        /// Wilson-score lower bound (tie-break: upper bound) so the
        /// most-confidently-flaky scenario floats to the top.
        #[arg(long, default_value_t = false)]
        flakes_only: bool,
        /// With `--baseline-show`, render the `.pending` sidecar
        /// instead of the canonical baseline. Use to inspect a partial
        /// baseline before deciding to promote or discard it.
        #[arg(long, default_value_t = false)]
        pending: bool,
        /// Promote `.itest-baseline.toml.pending` into the canonical
        /// baseline. The previous canonical `current` per scenario is
        /// pushed to `history`; the partial marker is stripped. Run
        /// after inspecting a pending file from an interrupted
        /// `--update-baseline` run.
        #[arg(long, default_value_t = false)]
        promote_pending: bool,
        /// Delete `.itest-baseline.toml.pending` without promoting.
        /// Idempotent (no-op if no pending file exists).
        #[arg(long, default_value_t = false)]
        discard_pending: bool,
        /// Rebuild the pending baseline from a per-run history
        /// directory's NDJSON. Use when the in-process pending write
        /// was lost (e.g. process killed mid-iteration). Path is the
        /// run directory, e.g. `.itest-runs/2026-06-08T12-30-15Z`.
        /// Refuses to overwrite an existing pending file.
        #[arg(long, value_name = "RUN_DIR")]
        recover_pending: Option<std::path::PathBuf>,
        /// Retroactively promote a completed run as the canonical
        /// baseline. `--adopt-run` alone picks the most recent
        /// `.itest-runs/<ts>/` directory; pass an explicit path to
        /// adopt a specific run. Useful when you ran `cargo xtask
        /// itest --repeat N` without `--update-baseline` and want
        /// to keep the results. Previous canonical entries are
        /// pushed to history.
        #[arg(
            long,
            value_name = "RUN_DIR",
            num_args = 0..=1,
            default_missing_value = "@latest",
        )]
        adopt_run: Option<String>,
        /// Prune `.itest-runs/` to the most recent N directories.
        /// Pass with `--keep-last N`. Per-run NDJSON, metadata, and
        /// captured failure logs in older runs are removed.
        #[arg(long, default_value_t = false)]
        prune_runs: bool,
        /// Number of run directories to retain when `--prune-runs` is set.
        /// Ignored otherwise. `0` removes everything.
        #[arg(long, default_value_t = 20)]
        keep_last: usize,
        /// Render the canonical baseline as Prometheus textfile-format
        /// metrics at the given path. For `node_exporter --collector.textfile`
        /// scraping into Grafana. Atomic write.
        #[arg(long, value_name = "PATH")]
        export_prom: Option<std::path::PathBuf>,
        /// Push the canonical baseline live to an OTLP/HTTP metrics
        /// receiver. Pass `--push-otlp` alone to target the bundled
        /// stack's Prometheus receiver at
        /// `http://127.0.0.1:9090/api/v1/otlp`, or pass an explicit
        /// endpoint URL (the receiver root — `/v1/metrics` is
        /// appended automatically). Useful in CI / cron / a post-run
        /// hook to land flake-rate data in Grafana without a
        /// textfile-collector.
        #[arg(
            long,
            value_name = "ENDPOINT",
            num_args = 0..=1,
            default_missing_value = "http://127.0.0.1:9090/api/v1/otlp",
        )]
        push_otlp: Option<String>,
        /// Disable the end-of-run auto-push. By default, after the
        /// test run completes, we try to push the canonical baseline
        /// to the bundled stack's OTLP receiver and warn if it's not
        /// reachable. Pass this in CI / scripts where the warning is
        /// noise.
        #[arg(long, default_value_t = false)]
        no_auto_push: bool,
        /// Number of scenarios to run in parallel. Default `10`
        /// (validated against an empirical A/B at this width — all
        /// scenarios stayed `consistent` against the sequential
        /// baseline). The runner partitions scenarios into
        /// Wfi-bounded (parallel at `--jobs` width) and Cpu-bounded
        /// (parallel at `--cpu-jobs` width, run as a separate pass
        /// after Wfi). Pass `--jobs 1` to force sequential. See
        /// `plans/itest-parallel-scenarios.md`.
        #[arg(
            long,
            default_value_t = 10,
            value_parser = clap::value_parser!(u32).range(1..=64),
        )]
        jobs: u32,
        /// Worker count for the Cpu-bound scenario batch. Defaults to
        /// `1` (fully serial): Cpu scenarios run real guest work — often
        /// multi-vcpu (e.g. the SMP workload + deflake storms use 2
        /// harts each) — so running them concurrently oversubscribes the
        /// host and makes timing-sensitive scenarios flaky. Raise it
        /// explicitly only when you know the box has the cores to spare.
        #[arg(
            long,
            default_value_t = 1,
            value_parser = clap::value_parser!(u32).range(1..=64),
        )]
        cpu_jobs: u32,
        /// Filter scenarios by classification. `--profile wfi` runs
        /// only wfi-bounded scenarios; `--profile cpu` runs only
        /// the Cpu-bound set. Useful for isolating which batch is
        /// driving wall-clock or flake behaviour while tuning
        /// `--jobs` / `--cpu-jobs`.
        #[arg(long, value_name = "PROFILE")]
        profile: Option<ProfileFilter>,
        /// How much frame transcript to persist for each failed
        /// scenario, as a `fail-<scenario>-<n>.capture.json` sidecar.
        /// `summary` keeps the classifier's summary record only; `tail`
        /// (default) adds the last ~64 frames; `full` adds every frame
        /// from the iteration. The summary record itself (outcome, frame
        /// counts, per-hart timestamps, histogram) is always captured
        /// regardless of level.
        #[arg(long, value_name = "LEVEL", default_value_t = CaptureArg::Tail)]
        capture: CaptureArg,
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

/// Failure-capture transcript depth for `cargo xtask itest --capture`.
/// Maps to `itest_harness::CaptureLevel`.
#[derive(Clone, Copy, Debug, ValueEnum)]
enum CaptureArg {
    /// Summary record only — no frame transcript.
    Summary,
    /// Summary + the last ~64 frames (default).
    Tail,
    /// Summary + every frame from the iteration.
    Full,
}

impl std::fmt::Display for CaptureArg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            CaptureArg::Summary => "summary",
            CaptureArg::Tail => "tail",
            CaptureArg::Full => "full",
        };
        f.write_str(s)
    }
}

impl From<CaptureArg> for itest_harness::CaptureLevel {
    fn from(arg: CaptureArg) -> Self {
        match arg {
            CaptureArg::Summary => itest_harness::CaptureLevel::Summary,
            CaptureArg::Tail => itest_harness::CaptureLevel::Tail,
            CaptureArg::Full => itest_harness::CaptureLevel::Full,
        }
    }
}

/// Scenario classification filter for `cargo xtask itest --profile`.
/// Maps to `itest_harness::CpuProfile`. The variant set is open —
/// add new ones (e.g. `Smp`, `Deflake`) as more useful axes emerge.
#[derive(Clone, Copy, Debug, ValueEnum)]
enum ProfileFilter {
    /// Only wfi-bounded scenarios.
    Wfi,
    /// Only cpu-bound scenarios (the `Scenario::cpu_bound` set).
    Cpu,
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
        Cmd::Test => itest::run_unit_tests(),
        Cmd::Itest {
            scenario,
            repeat,
            force,
            skip_unit_tests,
            update_baseline,
            fail_fast,
            baseline_show,
            include_history,
            flakes_only,
            pending,
            promote_pending,
            discard_pending,
            recover_pending,
            adopt_run,
            prune_runs,
            keep_last,
            export_prom,
            push_otlp,
            no_auto_push,
            jobs,
            cpu_jobs,
            profile,
            capture,
        } => {
            if let Some(endpoint) = push_otlp {
                return itest::push_otlp_metrics(endpoint);
            }
            if let Some(out) = export_prom {
                return itest::export_prom(out);
            }
            if prune_runs {
                return itest::prune_runs(keep_last);
            }
            if let Some(dir) = recover_pending {
                return itest::recover_pending(dir);
            }
            if let Some(target) = adopt_run {
                let run_dir = if target == "@latest" {
                    None
                } else {
                    Some(std::path::PathBuf::from(target))
                };
                return itest::adopt_run(run_dir);
            }
            if promote_pending {
                return itest::promote_pending();
            }
            if discard_pending {
                return itest::discard_pending();
            }
            if baseline_show {
                return itest::show_baseline(include_history, flakes_only, pending);
            }
            if !skip_unit_tests {
                let unit = itest::run_unit_tests();
                if unit != ExitCode::SUCCESS {
                    eprintln!(
                        "\nunit tests failed — skipping integration. Pass --skip-unit-tests to force."
                    );
                    return unit;
                }
                eprintln!();
            }
            let profile_filter = profile.map(|p| match p {
                ProfileFilter::Wfi => itest_harness::CpuProfile::Wfi,
                ProfileFilter::Cpu => itest_harness::CpuProfile::Cpu,
            });
            itest::set_capture_level(capture.into());
            itest::run(
                scenario.as_deref(),
                repeat,
                force,
                update_baseline,
                fail_fast,
                !no_auto_push,
                jobs,
                cpu_jobs,
                profile_filter,
            )
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
    if status.success() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

fn debug(features: &str) -> ExitCode {
    // Build with whatever features the caller requested.
    let features_vec: Vec<&str> = if features.is_empty() {
        Vec::new()
    } else {
        features.split(',').collect()
    };
    let status = qemu::build_kernel(&features_vec).expect("failed to invoke cargo");
    if !status.success() {
        return ExitCode::from(1);
    }

    // Clean up any stale telemetry socket from a previous run.
    let _ = std::fs::remove_file(TELEMETRY_SOCKET);

    // `wait=off` so the chardev doesn't block — we want QEMU to halt
    // at start (via -S) waiting for the debugger, not waiting for a
    // telemetry client. Telemetry is irrelevant in debug runs.
    let chardev_arg = format!("socket,path={TELEMETRY_SOCKET},server=on,wait=off,id=telemetry");

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
    eprintln!(
        "  riscv64-elf-gdb target/{}/debug/kernel",
        qemu::KERNEL_TARGET
    );
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
    let chardev_arg = format!("socket,path={TELEMETRY_SOCKET},server=on,wait=on,id=telemetry");

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
            "-p",
            "kernel-core",
            "-p",
            "protocol",
            "-p",
            "collector",
            "-p",
            "xtask",
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
        .args([
            "mutants",
            "-p",
            "collector",
            "-p",
            "protocol",
            "-p",
            "kernel-core",
            "--features",
            "protocol/std",
        ])
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
