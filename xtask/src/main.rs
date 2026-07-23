use std::process::{Command, ExitCode};

use clap::{Parser, Subcommand};

// `qemu` moved to the `xtask-qemu` crate (extracted so scenario edits don't
// recompile it). Aliased here so every existing `crate::qemu::…` reference in
// the submodules keeps resolving unchanged.
use xtask_qemu as qemu;
// Standalone commands moved to the `xtask-cmds` crate; re-imported at root so
// existing `crate::links::…` / `crate::source::…` references (and the dispatch
// calls below) keep resolving unchanged.
use xtask_cmds::{audit, links, loc, measure, snip};

mod plan;

const COLLECTOR_BIN: &str = "target/debug/collector";
const TELEMETRY_SOCKET: &str = "/tmp/snitch-telemetry.sock";

/// Orchestration commands for the `SnitchOS` workspace.
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
    /// Build the VisionFive 2 (`vf2`) kernel and emit a RISC-V `Image`
    /// (`snitchos.img`) for U-Boot `booti`. The 64-byte Image header is embedded
    /// at the start of the kernel by `entry.S`, so this is a straight ELF→binary
    /// `objcopy`. Load it on the board with `booti` (see the printed hint).
    Image,
    /// Everything that runs the kernel under the snemu emulator: the
    /// meta-loop driver (`boot`), the QEMU differential oracle (`diff`), the
    /// snapshot/fork harness (`fork`), the measurement spine (`bench`), and the
    /// guest instret profiler (`profile`).
    ///
    /// The everyday test command is not in here: it was promoted out to `itest`,
    /// which is snemu-backed by default (see
    /// plans/xtask-surface-consolidation.md, Step 2.1).
    Snemu {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Build the kernel and run it in QEMU.
    ///
    /// Use `--workload <name>` to boot a runtime-selected workload for
    /// live measurement / demos (e.g. `boot --workload smp` then
    /// `cargo xtask reader` in another terminal). `--features <feat>`
    /// builds a feature-flagged kernel directly.
    Boot {
        /// Cargo features to enable on the kernel build, comma-separated.
        #[arg(long, default_value = "")]
        features: String,
        /// Select a runtime workload by name (e.g. `smp`, `frame-oom`,
        /// `mutex-storm`). Implies `--features itest-workloads` and
        /// passes `workload=<name>` as the kernel bootarg. With no
        /// `--workload`, the kernel runs the default demo.
        #[arg(long)]
        workload: Option<String>,
        /// Batches per yield for the producer/consumer (`burst=N`
        /// bootarg). Higher = more queue contention (e.g. 65536 for the
        /// contended-Mutex Grafana view). Only meaningful with
        /// `--workload smp`/`smp-spsc`.
        #[arg(long)]
        burst: Option<usize>,
        /// Add `-device ramfb`, giving the guest an `etc/ramfb` fw_cfg
        /// file to bring up the framebuffer.
        #[arg(long)]
        ramfb: bool,
        /// Show an actual QEMU window instead of the default headless
        /// (`-nographic`) run. Takes a `-display` backend, e.g. `cocoa`
        /// (macOS) or `gtk` (Linux). Combine with `--ramfb` to see the
        /// framebuffer.
        #[arg(long)]
        display: Option<String>,
    },
    /// Boot a runtime-selected workload, capture its telemetry for a
    /// fixed window, and print steady-state stats (throughput, lock-wait
    /// fraction, queue depth). Replicable version of the
    /// boot+reader+parse measurement. See
    /// `docs/v0.6-mutex-vs-spsc-measurements.md`.
    Measure {
        /// Workload to select and measure (e.g. `smp`).
        #[arg(long)]
        workload: String,
        /// Capture window in seconds (wall clock).
        #[arg(long, default_value_t = 30)]
        seconds: u64,
        /// Seconds of the consumed series to skip as boot transient.
        #[arg(long, default_value_t = 6.0)]
        warmup: f64,
        /// Kernel timebase in Hz (QEMU `virt` = 10 MHz).
        #[arg(long, default_value_t = measure::DEFAULT_TIMEBASE_HZ)]
        timebase_hz: u64,
        /// Batches per yield (`burst=N` bootarg). Higher = more queue
        /// contention. Omit for the default low-contention shape.
        #[arg(long)]
        burst: Option<usize>,
        /// Emit a markdown table (for pasting into the measurements doc).
        #[arg(long, default_value_t = false)]
        markdown: bool,
    },
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
    /// Run mutation testing against the host-testable crates (collector,
    /// protocol, the `kernel-*` crates, hitch, stitch) — one cargo-mutants run
    /// per crate, each with the features its own tests need.
    ///
    /// Name a crate to mutate just that one, which is what you want during the
    /// MUTATE step: `cargo xtask mutants kernel-proc -- -f kernel-proc/src/elf.rs`.
    /// Trailing args are forwarded to cargo-mutants, e.g. `-- -j 4`.
    Mutants {
        /// Mutate only this crate (default: every crate, in turn).
        krate: Option<String>,
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
    /// Run every host-side check across the workspace: the unit tests
    /// (the `kernel-*` crates, `protocol --features std`, `collector`, …), the
    /// loom model-check tests (a separate `--cfg loom` compilation), the
    /// generated-diagram drift check, and the doc-link check. Fast (~1s).
    /// Doesn't touch QEMU.
    Test,
    /// Check that every relative `.md` link in the repo resolves.
    ///
    /// Also runs inside `cargo xtask test`; standalone here because it's
    /// instant and the thing you want right after a `git mv`. A moved doc
    /// breaks links both ways: inbound links still name the old path, and the
    /// moved file's own `../` links now resolve one directory too high.
    Links,
    /// Run kernel integration tests in QEMU.
    ///
    /// Runs integration only — it does **not** run the host-side checks
    /// first. Compose the gate explicitly:
    /// `cargo xtask test && cargo xtask itest`. (`xtask test` is the
    /// unit tests *plus* the loom model-checks and the generated-diagram
    /// drift check.)
    ///
    /// With no scenario name, runs every known integration
    /// scenario and reports a pass/fail summary. Use `--repeat N`
    /// to run the suite N times back-to-back; an aggregate flake
    /// report lists scenarios that failed in at least one run.
    ///
    /// If `qemu-system-riscv64` processes are already running (e.g. a
    /// stale QEMU from a prior `cargo xtask boot` or debug session, which
    /// could compete for host CPU and cause flakes), the run warns about
    /// them but does not kill them — the itest lock already prevents
    /// itest-vs-itest races. Kill stragglers manually if needed.
    Itest {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Inspect and manage the integration-test baseline
    /// (`.itest-baseline.toml`) and per-run history (`.itest-runs/`).
    /// These are the management verbs that used to be `itest` flags.
    Baseline {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Generate a mermaid diagram from a source of truth into
    /// `docs/generated/`. Diagrams render in GitHub markdown in-diff, so the
    /// committed artifacts are reviewable. See `docs/diagrams-design.md`.
    Diagram {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Count lines of code across the workspace, split by crate and
    /// by production vs test lines.
    Loc,
    /// Gather crate-audit evidence for one crate: per-`pub`-symbol caller
    /// table (ext/int/test), zero-caller candidates, debt markers, and unused
    /// dependencies (`cargo machete`). Mechanical input for the `crate-audit`
    /// skill — flags candidates, never deletes. Counts are a lower bound on
    /// deadness (name collisions over-count), so verify candidates against the
    /// design docs (rule 6) before acting. Requires `cargo-machete` on `PATH`.
    Audit {
        /// Crate (workspace dir) to audit, e.g. `kernel-core`.
        crate_name: String,
        /// Emit machine-readable JSON instead of the text table.
        #[arg(long, default_value_t = false)]
        json: bool,
        /// Include ≤2-char identifiers (off by default — single/double-letter
        /// names match as words everywhere and flood the counts with noise).
        #[arg(long, default_value_t = false)]
        include_short: bool,
    },
    /// Build the kernel and run it under QEMU with a GDB stub (`-s -S`).
    /// QEMU halts at start and listens on localhost:1234; attach with
    /// lldb or riscv64-unknown-elf-gdb from another terminal. Prints
    /// ready-to-copy attach commands.
    ///
    /// Use `--features <feat>` to build feature-flagged kernels (e.g.
    /// `--features deflake-spawn-storm` to debug a storm scenario).
    Debug {
        /// Cargo features to enable on the kernel build, comma-separated.
        #[arg(long, default_value = "")]
        features: String,
    },
    /// Print a failed itest capture's frame transcript from `.itest-runs/`, so a
    /// capture can be inspected without hand-parsing JSON. Defaults to the most
    /// recent run.
    #[command(name = "itest-show")]
    ItestShow {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Sonnet-assisted staging for parallel-agent workflows. You write the
    /// commit message; Sonnet picks which of the many concurrent working-tree
    /// changes belong to it. Three steps:
    ///   `snip "<msg>"`   propose (asks claude, writes .git/snip-plan.json)
    ///   `snip --stage`   `git add` the proposed files (then inspect `git diff --cached`)
    ///   `snip --commit`  `git commit` the plan's message
    ///
    /// The agent never runs git — this xtask binary does, after you approve.
    Snip {
        /// Commit message to triage for. Omit when using --stage or --commit.
        message: Option<String>,
        /// Diffstat-free payload (filenames + status only). Fastest, coarser.
        #[arg(long, default_value_t = false)]
        fast: bool,
        /// Propose and immediately `git add` (still leaves commit separate).
        #[arg(long, default_value_t = false)]
        yes: bool,
        /// Don't auto-stage even when the proposal is high-confidence throughout.
        /// (By default a fully-high-confidence proposal is staged automatically.)
        #[arg(long, default_value_t = false)]
        no_auto: bool,
        /// Opt into two-pass lean-first triage (paths-only pass, then full diffs
        /// only for undecided files). Rarely cheaper: `claude -p` has a ~57k-token
        /// fixed baseline per call, so the extra call usually outweighs the diffs
        /// it skips. Only worth it for a huge diff payload. Default: single pass.
        #[arg(long, default_value_t = false)]
        lean: bool,
        /// `git add` the files from the last proposal.
        #[arg(long, default_value_t = false)]
        stage: bool,
        /// `git commit` the last proposal's message, then clear the plan.
        #[arg(long, default_value_t = false)]
        commit: bool,
        /// Proceed despite working-tree drift or low overall confidence.
        #[arg(long, default_value_t = false)]
        force: bool,
        /// Pass `--no-verify` through to `git commit`.
        #[arg(long, default_value_t = false)]
        no_verify: bool,
    },
}

#[derive(Subcommand, Clone, Copy)]
enum StackCmd {
    /// `docker compose up -d` the stack.
    Up,
    /// `docker compose down` the stack.
    Down,
    /// `docker compose logs -f` the stack.
    Logs,
}

/// `cargo run -p xtask` injects package-scoped vars (`CARGO_MANIFEST_DIR`,
/// `CARGO_PKG_*`, …) into our process environment. Every child `cargo` we spawn
/// inherits them, so a dependency shared with a child build (e.g. `ring`,
/// pulled by the unit-test crates) bakes xtask's manifest dir into its
/// build-script fingerprint. A subsequent *direct* shell `cargo build` sees
/// those vars absent, marks the fingerprint dirty, and rebuilds — and the next
/// xtask run dirties it the other way. The result is ring/rustls/ureq/xtask
/// recompiling on every alternation between `cargo xtask …` and a plain build.
///
/// Strip the leaked vars once, before spawning anything, so children see the
/// same environment a shell build would. xtask reads its own package metadata
/// only via compile-time `env!`, which `remove_var` does not affect.
/// Whether an inherited env var must be scrubbed before xtask spawns child
/// cargo builds. Two families leak from the `cargo run` that launched xtask and
/// corrupt those children:
/// - **`CARGO_*` per-package vars** (`CARGO_MANIFEST_DIR`, `CARGO_PKG_*`, …) —
///   make child builds think they're xtask, thrashing the build cache.
/// - **`RUSTFLAGS` / `CARGO_ENCODED_RUSTFLAGS`** — a *release* `cargo run
///   --release -p xtask` leaks xtask's host rustflags into the spawned
///   `cargo build -p kernel`, whose host `build.rs` compile then dies with
///   "Only small, tiny and large code models are allowed on `AArch64`" (the
///   kernel's riscv `code-model=medium` flag applied to a host compile). The
///   kernel's real flags come from its target-scoped `.cargo/config.toml`, not
///   this inherited value, so dropping it is safe and unblocks release xtask.
fn should_scrub_env_key(key: &str) -> bool {
    key == "CARGO_MANIFEST_DIR"
        || key == "CARGO_MANIFEST_PATH"
        || key == "CARGO_CRATE_NAME"
        || key == "CARGO_BIN_NAME"
        || key == "CARGO_PRIMARY_PACKAGE"
        || key.starts_with("CARGO_PKG_")
        || key == "RUSTFLAGS"
        || key == "CARGO_ENCODED_RUSTFLAGS"
}

fn scrub_inherited_cargo_env() {
    let leaked: Vec<String> = std::env::vars()
        .map(|(key, _)| key)
        .filter(|key| should_scrub_env_key(key))
        .collect();
    for key in leaked {
        // SAFETY: called as the first statement of `main`, before any thread is
        // spawned, so there is no concurrent access to the process environment.
        unsafe { std::env::remove_var(key) };
    }
}

/// Characterisation of the CLI surface — the net the consolidation work
/// (plans/xtask-surface-consolidation.md) renames things across. These assert
/// what the tree accepts *today*; they are not a design statement. When a step
/// deliberately changes the surface, the failing test here is the point, and the
/// step updates it.
#[cfg(test)]
mod mutant_plan_tests {
    use super::{NOT_MUTATED, mutant_plan};
    use crate::plan::{EXTRA_TEST_ARGS, NOT_HOST_TESTED};

    const NO_ARGS: &[&str] = &[];

    /// The anti-drift property: a crate joins the mutation gate by existing, not
    /// by someone remembering to list it. The old hardcoded allow-list is exactly
    /// how `snemu`, `supervision`, `ramfs` and ten others were never mutated.
    #[test]
    fn a_new_host_crate_is_mutated_by_default() {
        let plan = mutant_plan(&["brand-new-crate"], &[], &[], &[]).expect("valid plan");
        assert_eq!(plan, vec![("brand-new-crate", NO_ARGS)]);
    }

    #[test]
    fn a_riscv_only_crate_is_not_mutated() {
        let plan = mutant_plan(&["kernel", "collector"], &[("kernel", "riscv only")], &[], &[])
            .expect("valid plan");
        assert_eq!(plan, vec![("collector", NO_ARGS)]);
    }

    #[test]
    fn a_deliberately_exempt_crate_is_not_mutated() {
        let plan = mutant_plan(&["snemu", "collector"], &[], &[("snemu", "too big")], &[])
            .expect("valid plan");
        assert_eq!(plan, vec![("collector", NO_ARGS)]);
    }

    /// The feature args a crate's own suite needs are the same ones cargo-mutants
    /// needs to run that suite — one table, not two.
    #[test]
    fn feature_args_come_from_the_test_gates_table() {
        let plan = mutant_plan(&["protocol"], &[], &[], &[("protocol", &["--features", "std"])])
            .expect("valid plan");
        assert_eq!(plan, vec![("protocol", &["--features", "std"] as &[&str])]);
    }

    /// A renamed or deleted crate must not leave a silent entry behind.
    #[test]
    fn an_exemption_naming_a_departed_crate_is_an_error() {
        let err = mutant_plan(&["collector"], &[], &[("kernel-core", "gone")], &[])
            .expect_err("stale exemption must fail");
        assert!(err.contains("kernel-core"), "error should name the stale entry: {err}");
    }

    /// Characterisation: deriving the list must not silently change *which* crates
    /// get mutated. This is the exact set the hardcoded `MUTANT_CRATES` named.
    /// Changing it is a deliberate act — enrolling a drift candidate from
    /// `NOT_MUTATED` should fail here first.
    #[test]
    fn the_derived_plan_matches_the_previously_hardcoded_set() {
        let members = crate::plan::workspace_members().expect("cargo metadata");
        let names: Vec<&str> = members.iter().map(String::as_str).collect();
        let plan = mutant_plan(&names, NOT_HOST_TESTED, NOT_MUTATED, EXTRA_TEST_ARGS)
            .expect("committed lists are current");

        let mut got: Vec<&str> = plan.iter().map(|(name, _)| *name).collect();
        got.sort_unstable();
        assert_eq!(got, vec![
            "collector",
            "hitch",
            "hitch-pod",
            "kernel-boot",
            "kernel-devices",
            "kernel-mem",
            "kernel-obs",
            "kernel-proc",
            "protocol",
            "stitch",
        ]);
    }
}

#[cfg(test)]
mod cli_surface_tests {
    use super::{Cli, Cmd};
    use clap::{CommandFactory, Parser};

    /// clap's own consistency check for the lean surface.
    #[test]
    fn the_clap_definition_is_internally_consistent() {
        Cli::command().debug_assert();
    }

    /// Every top-level verb parses — the lean commands natively, and the
    /// snemu-linked ones (`itest`/`snemu`/`baseline`/`itest-show`) as raw
    /// passthroughs forwarded to `xtask-itest`.
    #[test]
    fn every_top_level_command_parses() {
        const MINIMAL_ARGV: &[&[&str]] = &[
            &["build"],
            &["snemu", "boot"],
            &["boot"],
            &["measure", "--workload", "smp"],
            &["collect"],
            &["reader"],
            &["mutants"],
            &["clippy"],
            &["stack", "up"],
            &["test"],
            &["itest"],
            &["baseline", "show"],
            &["diagram", "deps"],
            &["loc"],
            &["audit", "kernel-mem"],
            &["debug"],
            &["itest-show"],
            &["snip", "a message"],
        ];
        for argv in MINIMAL_ARGV {
            let full: Vec<&str> = std::iter::once("xtask").chain(argv.iter().copied()).collect();
            assert!(
                Cli::try_parse_from(&full).is_ok(),
                "top-level command should parse: {argv:?}",
            );
        }
    }

    /// The snemu-linked verbs are raw passthroughs: lean `xtask` captures their
    /// argv verbatim and forwards it to `xtask-itest`, which does the real parsing
    /// and validation. So the flags are deliberately *not* checked here — keeping
    /// them out of the lean `Cli` is what keeps `snemu` out of the tool.
    #[test]
    fn delegated_commands_capture_their_argv_verbatim() {
        let cli = Cli::try_parse_from(["xtask", "itest", "--engine", "qemu", "sched"])
            .expect("itest passthrough parses");
        let Cmd::Itest { args } = cli.cmd else { panic!("expected Itest") };
        assert_eq!(args, ["--engine", "qemu", "sched"]);

        let cli = Cli::try_parse_from(["xtask", "snemu", "diff", "--all"])
            .expect("snemu passthrough parses");
        let Cmd::Snemu { args } = cli.cmd else { panic!("expected Snemu") };
        assert_eq!(args, ["diff", "--all"]);

        let cli = Cli::try_parse_from(["xtask", "baseline", "prune", "--keep-last", "5"])
            .expect("baseline passthrough parses");
        let Cmd::Baseline { args } = cli.cmd else { panic!("expected Baseline") };
        assert_eq!(args, ["prune", "--keep-last", "5"]);
    }

    /// A verb we never had must not parse.
    #[test]
    fn an_unknown_command_is_rejected() {
        assert!(Cli::try_parse_from(["xtask", "not-a-real-command"]).is_err());
    }

    /// `stack` stays a native subcommand group in lean `xtask`, so it still
    /// rejects a missing or bogus member. (`diagram` is now a passthrough — its
    /// validation moved to `xtask-itest`.)
    #[test]
    fn native_subcommand_groups_require_a_valid_member() {
        assert!(Cli::try_parse_from(["xtask", "stack"]).is_err());
        assert!(Cli::try_parse_from(["xtask", "stack", "sideways"]).is_err());
    }
}

#[cfg(test)]
mod env_scrub_tests {
    use super::should_scrub_env_key;

    #[test]
    fn scrubs_the_leaking_cargo_and_rustflags_vars() {
        // The per-package CARGO_* leak (cache thrash) and the release rustflags
        // leak (kernel host-build failure) must both be scrubbed.
        assert!(should_scrub_env_key("CARGO_MANIFEST_DIR"));
        assert!(should_scrub_env_key("CARGO_PKG_VERSION"));
        assert!(should_scrub_env_key("RUSTFLAGS"));
        assert!(should_scrub_env_key("CARGO_ENCODED_RUSTFLAGS"));
    }

    #[test]
    fn leaves_unrelated_and_needed_vars_alone() {
        // Don't over-scrub: PATH and cargo's home/target config are needed by
        // the child builds.
        assert!(!should_scrub_env_key("PATH"));
        assert!(!should_scrub_env_key("CARGO_HOME"));
        assert!(!should_scrub_env_key("CARGO_TARGET_DIR"));
    }
}

/// Forward a snemu-linked subcommand to the `xtask-itest` binary, which owns the
/// emulator-facing half (`itest` / `snemu` / `baseline` / `itest-show`). Split out
/// so the lean `xtask` tool no longer links `snemu` — see
/// plans/xtask-lean-test-binary.md. Argv is passed through verbatim; `xtask-itest`
/// does the real parsing and validation.
fn delegate_itest(subcommand: &str, args: &[String]) -> ExitCode {
    let status = Command::new("cargo")
        .args(["run", "-p", "xtask-itest", "--", subcommand])
        .args(args)
        .status()
        .expect("failed to invoke cargo run -p xtask-itest");
    match status.code() {
        Some(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        None => ExitCode::from(1),
    }
}

fn main() -> ExitCode {
    scrub_inherited_cargo_env();
    match Cli::parse().cmd {
        Cmd::Build => build(),
        Cmd::Image => image(),
        Cmd::Snemu { args } => delegate_itest("snemu", &args),
        Cmd::Boot { features, workload, burst, ramfb, display } => {
            boot(&features, workload.as_deref(), burst, ramfb, display.as_deref())
        }
        Cmd::Measure { workload, seconds, warmup, timebase_hz, burst, markdown } => {
            measure::measure(&workload, seconds, warmup, timebase_hz, burst, markdown)
        }
        Cmd::Mutants { krate, args } => run_mutants(krate.as_deref(), &args),
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
        Cmd::Test => plan::run_unit_tests(),
        Cmd::Links => links::check(),
        Cmd::ItestShow { args } => delegate_itest("itest-show", &args),
        Cmd::Itest { args } => delegate_itest("itest", &args),
        Cmd::Baseline { args } => delegate_itest("baseline", &args),
        Cmd::Diagram { args } => delegate_itest("diagram", &args),
        Cmd::Loc => loc::run(),
        Cmd::Audit { crate_name, json, include_short } => {
            audit::run(&crate_name, json, include_short)
        }
        Cmd::Debug { features } => debug(&features),
        Cmd::Snip { message, fast, yes, no_auto, lean, stage, commit, force, no_verify } => {
            if commit {
                snip::commit(no_verify)
            } else if stage {
                snip::stage(force)
            } else if let Some(message) = message {
                snip::propose(&message, &snip::ProposeOpts { fast, yes, no_auto, force, lean })
            } else {
                eprintln!(
                    "snip: provide a commit message to propose, or --stage / --commit to finalize"
                );
                ExitCode::from(1)
            }
        }
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
    // The kernel's build.rs builds + embeds the userspace programs itself.
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

    let status = qemu::base_command(&chardev_arg, qemu::DEFAULT_RAM_MB, qemu::OptLevel::Low)
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

/// Build the `vf2` kernel and objcopy it to a flat RISC-V `Image` (`snitchos.img`)
/// for U-Boot `booti` on the VisionFive 2. The 64-byte Image header is embedded at
/// the start of the kernel by `entry.S`, so this is a straight ELF→binary copy —
/// no header to prepend, so the kernel's link addresses stay intact.
fn image() -> ExitCode {
    // Board build: RAM base 0x4000_0000 (the `vf2` feature), no test workloads.
    let status = qemu::build_kernel(&["vf2"]).expect("failed to invoke cargo");
    if !status.success() {
        return ExitCode::from(1);
    }
    let elf = qemu::kernel_bin(false);
    let out = "snitchos.img";
    // `rust-objcopy` (cargo-binutils) wraps `llvm-objcopy`; on PATH after
    // `cargo install cargo-binutils` + `rustup component add llvm-tools`.
    match Command::new("rust-objcopy")
        .args(["-O", "binary", elf, out])
        .status()
    {
        Ok(s) if s.success() => {
            eprintln!("wrote {out} — RISC-V Image for U-Boot `booti`.");
            eprintln!("At the VisionFive 2's U-Boot prompt (TFTP the file first):");
            eprintln!("  tftpboot 0x40200000 snitchos.img");
            eprintln!("  booti 0x40200000 - ${{fdtcontroladdr}}");
            ExitCode::SUCCESS
        }
        Ok(_) => ExitCode::from(1),
        Err(e) => {
            eprintln!("rust-objcopy failed to launch: {e}");
            eprintln!("install: cargo install cargo-binutils && rustup component add llvm-tools");
            ExitCode::from(1)
        }
    }
}

fn boot(
    features: &str,
    workload: Option<&str>,
    burst: Option<usize>,
    ramfb: bool,
    display: Option<&str>,
) -> ExitCode {
    let mut features_vec: Vec<&str> = if features.is_empty() {
        Vec::new()
    } else {
        features.split(',').collect()
    };
    // `--workload`/`--burst` select runtime behaviour, which only exists
    // in `itest-workloads` builds — imply the feature so a bare
    // `boot --workload smp` just works.
    if (workload.is_some() || burst.is_some()) && !features_vec.contains(&"itest-workloads") {
        features_vec.push("itest-workloads");
    }
    let status = qemu::build_kernel(&features_vec).expect("failed to invoke cargo");
    if !status.success() {
        return ExitCode::from(1);
    }

    // Clean up any stale socket from a previous run so QEMU can bind.
    let _ = std::fs::remove_file(TELEMETRY_SOCKET);

    // wait=on blocks QEMU at startup until a telemetry client connects.
    // Run `cargo xtask collect` (or `cargo xtask reader`) in another
    // terminal to satisfy that wait.
    let chardev_arg = format!("socket,path={TELEMETRY_SOCKET},server=on,wait=on,id=telemetry");

    let mut cmd = qemu::base_command_ex(&chardev_arg, qemu::DEFAULT_RAM_MB, ramfb, display, qemu::OptLevel::Low);
    // Lands in /chosen/bootargs; `kmain` reads it to pick the runtime
    // workload + burst.
    let bootargs: Vec<String> = workload
        .map(|w| format!("workload={w}"))
        .into_iter()
        .chain(burst.map(|b| format!("burst={b}")))
        .collect();
    if !bootargs.is_empty() {
        cmd.args(["-append", &bootargs.join(" ")]);
    }
    let status = cmd
        .status()
        .expect("failed to invoke qemu-system-riscv64");
    if status.success() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

/// Lint the whole workspace, each crate for the target it actually builds for.
///
/// Both crate lists are **derived from `cargo metadata`**, not written down here.
/// An allow-list let a crate be silently never-linted by simple omission, which is
/// exactly what happened: `snemu`, `stitch`, `hitch` and eleven others were never
/// linted at all, and the riscv half was missing `snitchos-std` and `fs`. Deriving
/// them means a new crate is linted the moment it joins the workspace, and
/// `plan::NOT_HOST_TESTED` is the only way out — the same shape the host test gate
/// already uses, for the same reason.
///
/// Per-crate invocation (rather than one `-p a -p b …`) because the feature args a
/// crate needs are its own: `--features stitch/testing` is invalid for any package
/// that doesn't depend on stitch. Same lesson `MUTANT_CRATES` records.
fn run_clippy(extra_args: &[String]) -> ExitCode {
    let members = match plan::workspace_members() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("clippy: {e}");
            return ExitCode::from(1);
        }
    };
    let names: Vec<&str> = members.iter().map(String::as_str).collect();

    // Host-buildable crates: lint everything including tests. Reuses the test
    // gate's per-crate feature args — a crate that needs `--features std` to
    // compile its tests needs it to lint them too.
    let host_plan = match plan::unit_test_plan(&names, plan::NOT_HOST_TESTED, plan::EXTRA_TEST_ARGS) {
        Ok(plan) => plan,
        Err(e) => {
            eprintln!("clippy: {e}");
            return ExitCode::from(1);
        }
    };
    let host = host_plan.iter().all(|(crate_name, crate_args)| {
        let mut args = vec!["clippy", "-p", crate_name, "--all-targets"];
        args.extend_from_slice(crate_args);
        Command::new("cargo")
            .args(&args)
            .args(extra_args)
            .status()
            .expect("failed to invoke cargo clippy")
            .success()
    });

    // The kernel and the userspace crates only compile for bare-metal riscv.
    // No `--all-targets`: none has a host-buildable test target.
    let riscv_plan = match plan::riscv_only_plan(&names, plan::NOT_HOST_TESTED) {
        Ok(plan) => plan,
        Err(e) => {
            eprintln!("clippy: {e}");
            return ExitCode::from(1);
        }
    };
    let kernel = riscv_plan.iter().all(|crate_name| {
        Command::new("cargo")
            .args(["clippy", "-p", crate_name, "--target", qemu::KERNEL_TARGET])
            .args(extra_args)
            .status()
            .expect("failed to invoke cargo clippy")
            .success()
    });

    if host && kernel {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

/// Host crates the mutation gate deliberately does **not** mutate, each with the
/// reason. Every other host-tested crate is mutated: the gate derives its list
/// from `cargo metadata`, so a crate joins by existing and this list is the only
/// way out. Opting out is a decision someone has to write down.
///
/// The inverse (the allow-list this replaced) let a crate be silently never
/// mutated by simple omission — which is how every entry below marked *drift*
/// got here. Unlike the test and clippy gates, mutation is expensive enough that
/// blanket enrolment is a real cost, so the exemptions stay; what changes is that
/// they are now written down rather than implied by absence.
const NOT_MUTATED: &[(&str, &str)] = &[
    // Structural — mutation would produce noise, not signal.
    ("fs-core", "no test suite of its own — every mutant would survive unkilled"),
    ("hitch-derive", "no test suite of its own; its output is exercised through `hitch`"),
    // Cost — real value, prohibitive runtime.
    ("snemu", "~10k lines, and each mutant re-runs an emulator suite; runtime is the blocker, not the value"),
    ("xtask", "the harness that runs this gate — mutating the judge"),
    // Tooling — the xtask binary split (plans/xtask-lean-test-binary.md). Real
    // suites, but tooling; enrolling them expands the gate beyond the core, so
    // they're held as candidates like the subsystem crates below. `xtask-itest`
    // is additionally integration-tested — its scenarios/runner run under snemu
    // via `cargo xtask itest`, not the unit suite, so many mutants would survive
    // as noise. Enrol one by deleting its line and updating the
    // `the_derived_plan_matches_the_previously_hardcoded_set` characterisation.
    ("xtask-cmds", "tooling: real suite; enrolment candidate"),
    ("xtask-itest", "tooling: runner is integration-tested via `cargo xtask itest`; enrolment candidate"),
    ("xtask-qemu", "tooling: real suite; enrolment candidate"),
    ("xtask-snemu", "tooling: real suite; enrolment candidate"),
    // Drift — these have real suites and are unmutated by accident, not decision.
    // Enrolling one means deleting its line here and updating the
    // `the_derived_plan_matches_the_previously_hardcoded_set` characterisation test.
    ("diagram", "drift: has a real suite; enrolment candidate"),
    ("fs-proto", "drift: has a real suite; enrolment candidate"),
    ("itest-harness", "drift: has a real suite; enrolment candidate"),
    ("magnitude", "drift: has a real suite; enrolment candidate"),
    ("ramfs", "drift: has a real suite; enrolment candidate"),
    ("snip", "drift: has a real suite; enrolment candidate"),
    ("snitchos-abi", "drift: has a real suite; enrolment candidate"),
    ("snitchos-user-macros", "drift: has a real suite; enrolment candidate"),
    ("supervision", "drift: has a real suite; enrolment candidate"),
];

/// The crates the mutation gate mutates, plus the features each one's *own* test
/// suite needs: every host-tested crate, minus the riscv-only ones (no host suite
/// to kill mutants with) and minus [`NOT_MUTATED`].
///
/// The feature args come from the test gate's own table — the features a crate
/// needs to compile its suite are exactly the ones cargo-mutants needs to *run*
/// that suite, so there is one table rather than two to drift apart.
///
/// One invocation per crate, rather than one invocation naming them all. That
/// isn't ceremony: cargo-mutants narrows `cargo test` to the mutant's owning
/// package, so a workspace-wide `--features protocol/std,stitch/testing` is
/// invalid for any package that doesn't depend on stitch. It survived only
/// because an unscoped baseline builds every `-p` together; the moment you
/// scope a run (`-f one/file.rs`) — which is the normal way to use this during
/// the MUTATE step — the baseline failed with *"the package 'kernel-proc' does
/// not contain this feature: stitch/testing"*. Per-crate, the feature list is
/// always the one that crate actually has.
fn mutant_plan<'a>(
    members: &[&'a str],
    riscv_only: &[(&str, &str)],
    not_mutated: &[(&str, &str)],
    extra_args: &[(&'static str, &'static [&'static str])],
) -> Result<Vec<(&'a str, &'static [&'static str])>, String> {
    // Checked here rather than left to `unit_test_plan`, so a stale entry names
    // the list it actually lives in.
    let stale: Vec<&str> =
        not_mutated.iter().map(|(name, _)| *name).filter(|name| !members.contains(name)).collect();
    if !stale.is_empty() {
        return Err(format!(
            "mutation policy names crates that are not workspace members: {}. \
             Renamed or removed? Update NOT_MUTATED in xtask/src/main.rs.",
            stale.join(", ")
        ));
    }

    let excluded: Vec<(&str, &str)> = riscv_only.iter().chain(not_mutated.iter()).copied().collect();
    plan::unit_test_plan(members, &excluded, extra_args)
}

/// One cargo-mutants invocation. Returns `true` iff it passed.
fn run_mutants_for(name: &str, features: &[&str], extra_args: &[String]) -> bool {
    Command::new("cargo")
        .args(["mutants", "-p", name])
        .args(features)
        .args(extra_args)
        .status()
        .expect("failed to invoke cargo mutants")
        .success()
}

/// Mutate `only` (or every crate the mutation gate covers, in turn). Trailing
/// args go to cargo-mutants — scope a run with
/// `cargo xtask mutants kernel-proc -- -f kernel-proc/src/elf.rs`.
fn run_mutants(only: Option<&str>, extra_args: &[String]) -> ExitCode {
    let members = match plan::workspace_members() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("mutants: {e}");
            return ExitCode::from(1);
        }
    };
    let names: Vec<&str> = members.iter().map(String::as_str).collect();
    let plan = match mutant_plan(&names, plan::NOT_HOST_TESTED, NOT_MUTATED, plan::EXTRA_TEST_ARGS)
    {
        Ok(plan) => plan,
        Err(e) => {
            eprintln!("mutants: {e}");
            return ExitCode::from(1);
        }
    };

    let crates: Vec<(&str, &[&str])> = match only {
        Some(name) => match plan.iter().find(|(c, _)| *c == name) {
            Some(entry) => vec![*entry],
            None => {
                let known: Vec<&str> = plan.iter().map(|(c, _)| *c).collect();
                let exempt = NOT_MUTATED.iter().find(|(c, _)| *c == name);
                match exempt {
                    Some((_, reason)) => {
                        eprintln!("`{name}` is exempt from the mutation gate: {reason}");
                        eprintln!("Remove its NOT_MUTATED entry in xtask/src/main.rs to enrol it.");
                    }
                    None => eprintln!("unknown crate `{name}`; known: {}", known.join(", ")),
                }
                return ExitCode::from(2);
            }
        },
        None => plan.iter().map(|(name, args)| (*name, *args)).collect(),
    };

    for (name, features) in crates {
        eprintln!("=== mutants: {name} ===");
        if !run_mutants_for(name, features, extra_args) {
            return ExitCode::from(1);
        }
    }
    ExitCode::SUCCESS
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
