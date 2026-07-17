use std::process::{Command, ExitCode};

use clap::{Parser, Subcommand, ValueEnum};

mod audit;
mod diagram_cmd;
mod itest;
mod loc;
mod measure;
mod qemu;
mod snemu_bench;
mod snemu_diff;
mod snemu_profile;
mod snip;
mod source;

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
    /// Build the kernel and run it under snemu, streaming snemu's output. This
    /// is the meta-loop driver: it always rebuilds the real boot path, then
    /// reports where snemu stops.
    SnemuBoot {
        /// Cargo features to enable on the kernel build, comma-separated.
        #[arg(long, default_value = "")]
        features: String,
        /// Cap the run at N instruction steps (snemu's default is 50M).
        #[arg(long)]
        max_steps: Option<u64>,
        /// Dump every telemetry frame snemu decodes off the virtio-console.
        #[arg(long)]
        frames: bool,
        /// Select a runtime workload (e.g. `demo`, `smp`). Implies the
        /// `itest-workloads` kernel build and injects `workload=<name>` into the
        /// DTB's /chosen/bootargs.
        #[arg(long)]
        workload: Option<String>,
    },
    /// Differential oracle: boot the same kernel under snemu and QEMU and
    /// structurally diff their telemetry frame streams (timestamps normalized).
    SnemuDiff {
        /// snemu instruction-step budget (round-robin splits it across harts).
        #[arg(long, default_value_t = 150_000_000)]
        steps: u64,
        /// Seconds to collect QEMU telemetry before killing it.
        #[arg(long, default_value_t = 6)]
        qemu_secs: u64,
        /// Runtime workload to run under both emulators (e.g. `demo`, `smp`).
        /// Implies the `itest-workloads` build. Omit for the default `init` boot.
        #[arg(long)]
        workload: Option<String>,
        /// Sweep *every* workload and print an agree/disagree summary table.
        /// (Ignores `--workload`.)
        #[arg(long)]
        all: bool,
        /// With `--all`, sweep only the first N workloads (faster).
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Snapshot/fork harness: boot the common prefix once under snemu, then fork
    /// every workload from that snapshot (clone + DTB bootarg patch). snemu-only;
    /// proves boot amortization.
    ///
    /// Distinct from `snemu-itest --share-snapshots`, which shares a boot only
    /// among scenarios of the *same* workload. This forks one boot across
    /// *different* workloads via a layout-preserving DTB overwrite.
    SnemuFork {
        /// Per-workload step budget after the fork.
        #[arg(long, default_value_t = 20_000_000)]
        steps: u64,
    },
    /// Fidelity audit: replay every itest scenario's assertion against a
    /// snemu-produced frame stream (no QEMU) and report how many pass. Sizes the
    /// "can snemu back the itests" gap without rewriting scenarios.
    SnemuItest {
        /// Per-scenario snemu instruction-step budget. Passing scenarios
        /// short-circuit well under this; the budget only bounds failing ones and
        /// the slow OOM/cooperative workloads. 400M recovers the budget-sensitive
        /// scenarios (e.g. `sched-yield-round-trips`).
        #[arg(long, default_value_t = 400_000_000)]
        steps: u64,
        /// Audit only the first N scenarios (faster smoke).
        #[arg(long)]
        limit: Option<usize>,
        /// Audit only scenarios whose name contains this substring.
        #[arg(long)]
        only: Option<String>,
        /// Parallel host workers. Scenarios are independent (each owns its own
        /// snemu machine), so they fan out across cores. snemu is a pure
        /// interpreter (CPU-bound), so the sweet spot is the physical core
        /// count. Defaults to the machine's available parallelism; `1` forces
        /// serial. Results stay in deterministic report order regardless.
        #[arg(long, short = 'j')]
        jobs: Option<usize>,
        /// Disable `wfi` idle-skip (on by default). The A/B baseline: run the
        /// audit both ways and confirm fidelity + per-scenario instret are
        /// identical — idle-skip must change only speed, never telemetry.
        #[arg(long)]
        no_idle_skip: bool,
        /// Scenario packing order: `wall` (default — LPT by the previous run's
        /// wall-time, the true optimisation target but noisy), `instret` (LPT by
        /// prior instret — deterministic, reproducible), or `selection` (no packing —
        /// the A/B baseline). The report prints both counterfactuals regardless.
        #[arg(long, value_enum, default_value_t)]
        order: itest::snemu_audit::PackOrder,
        /// Optimization regime, with distinct failure modes — flick between them to
        /// localize a bug: `low` (debug, opt-0 — where scenarios depending on unbuilt
        /// work like supervision fail, the honest correctness test), `mid` (release
        /// kernel + opt-1 userspace — fast and currently green, the former `--release`),
        /// or `high` (release everywhere — surfaces the userspace opt≥2 UB class).
        #[arg(long, value_enum, default_value_t = qemu::OptLevel::Mid)]
        opt: qemu::OptLevel,
        /// Enable the native-op helper (tier-0.5 JIT): fast-path guest memset/memcpy
        /// (execute natively + charge the interpreter-equivalent instret). A/B it —
        /// on vs off must keep the suite green (fidelity), only faster.
        #[arg(long)]
        native_ops: bool,
        /// Enable the Tier-2 block JIT (M6): compile + run hot basic blocks. A/B it
        /// against off while ISA coverage expands — on vs off must stay green +
        /// byte-identical guest instret (the oracle), only faster.
        #[arg(long = "jit")]
        block_jit: bool,
        /// With `--jit`, disable the block executor's register caching (M6 inc 4) —
        /// the A/B baseline to isolate the caching's wall-time effect.
        #[arg(long)]
        no_reg_cache: bool,
        /// Enable the discovered-snapshot-tree collapse (off by default — the A/B
        /// baseline). Observe-only scenarios (empty branch key, learned from a prior
        /// run's persisted keys) of a workload share one forward run instead of each
        /// re-executing the identical deterministic guest; each replays a prefix of
        /// that shared stream truncated to its own budget, so verdicts are identical
        /// to the fork-per-scenario path. See `docs/snemu-itest-snapshot-tree-design.md`.
        #[arg(long)]
        share_snapshots: bool,
        /// Enable **Backend B** (native AArch64 codegen) for the block JIT — implies
        /// `--jit`. Host-only (arm64/macos); A/B it against off, which must stay green
        /// + byte-identical guest instret (the oracle), only faster.
        #[arg(long = "native-jit")]
        native_jit: bool,
        /// Enable the software **TLB** (Sv39 translation cache). A/B it against off,
        /// which must stay green + byte-identical guest instret (the oracle), only
        /// faster — this is the lever for the memory/translation pole.
        #[arg(long)]
        tlb: bool,
        /// Preset speedup bundle: `low` (idle-skip only), `med` (+native-ops +TLB),
        /// `hi` (+block JIT / Backend A — the fastest *portable*, **the default**),
        /// `extra` (+Backend B native codegen — experimental, host-only, currently
        /// slower). Individual `--jit`/`--tlb`/… flags layer on top. Pass
        /// `--speedup low` for the idle-skip-only A/B baseline.
        #[arg(long, value_enum, default_value = "hi")]
        speedup: itest::snemu_audit::SpeedLevel,
    },
    /// Guest instret profiler: boot a workload to the heartbeat checkpoint, then
    /// run under snemu with exact per-PC counting and report the top kernel
    /// functions by instructions retired. Answers *which code* a scenario spends
    /// its cycles in (e.g. a cross-hart spin-wait vs. real work), the per-function
    /// complement to `snemu-itest`'s per-scenario ruler.
    SnemuProfile {
        /// Workload to profile (implies the `itest-workloads` build). Omit for the
        /// default `init` boot.
        #[arg(long)]
        workload: Option<String>,
        /// Instructions to run (post-boot) under the profiler.
        #[arg(long, default_value_t = 400_000_000)]
        steps: u64,
        /// How many top functions to list.
        #[arg(long, default_value_t = 25)]
        top: usize,
        /// Profile the optimized (`--release`) kernel (matches the release itests).
        #[arg(long)]
        release: bool,
    },
    /// Measurement spine: run a workload under snemu N times and report guest
    /// MIPS + wall-clock spread over a deterministic instret. The "measure
    /// first" baseline every JIT tier is judged against.
    SnemuBench {
        /// Workload to measure (implies the `itest-workloads` build). Omit for
        /// the default `init` boot.
        #[arg(long)]
        workload: Option<String>,
        /// Instruction-step budget per run.
        #[arg(long, default_value_t = 50_000_000)]
        steps: u64,
        /// Number of timed runs (determinism check + wall-clock spread).
        #[arg(long, default_value_t = 5)]
        runs: u32,
        /// Sweep the four taxonomy classes (startup/compute/memory/trap-MMIO)
        /// and print a comparison table. Ignores `--workload`/`--steps`.
        #[arg(long)]
        taxonomy: bool,
        /// Like `--taxonomy`, plus a QEMU wall-clock baseline overlay (time to
        /// the shared 100-frame milestone, snemu vs QEMU). Ignores
        /// `--workload`/`--steps`.
        #[arg(long)]
        baseline: bool,
        /// Enable the Tier-1 decode cache (M5). A/B against the default (off) to
        /// read the speedup; instret stays identical (correctness).
        #[arg(long)]
        decode_cache: bool,
        /// Verify the decode cache is faithful: run each taxonomy workload with
        /// the cache off and on and assert identical telemetry. Overrides the
        /// other modes.
        #[arg(long)]
        verify_cache: bool,
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
    /// protocol, kernel-core, hitch). Trailing args are forwarded to
    /// cargo-mutants, e.g. `cargo xtask mutants -- -j 4`.
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
    /// If `qemu-system-riscv64` processes are already running (e.g. a
    /// stale QEMU from a prior `cargo xtask boot` or debug session, which
    /// could compete for host CPU and cause flakes), the run warns about
    /// them but does not kill them — the itest lock already prevents
    /// itest-vs-itest races. Kill stragglers manually if needed.
    Itest {
        /// Scenario name, or a comma-separated list (`a,b,c`). Omit to
        /// run all.
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
        /// `plans/legacy/itest-parallel-scenarios.md`.
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
        #[arg(long, value_name = "LEVEL", default_value_t = CaptureArg::Signal)]
        capture: CaptureArg,
        /// Scenario name(s) to exclude from the run. Repeatable and/or
        /// comma-separated: `--skip a --skip b` or `--skip a,b`. Applied
        /// after `--profile`. Useful to run a batch while leaving out a
        /// known-slow or known-failing scenario.
        #[arg(long, value_name = "SCENARIO", value_delimiter = ',')]
        skip: Vec<String>,
        /// Select scenarios by tag (union). Repeatable and/or
        /// comma-separated: `--tag userspace --tag smp` or
        /// `--tag userspace,smp` runs every scenario carrying either
        /// tag. A tag no scenario carries is an error (typo guard).
        /// Can't be combined with a positional scenario name.
        #[arg(long, value_name = "TAG", value_delimiter = ',')]
        tag: Vec<String>,
        /// Shared-boot mode: group scenarios by their `workload` and run
        /// each group against a single kernel boot (so the ~19 default-demo
        /// and ~12 userspace scenarios each boot QEMU once instead of N
        /// times). Much faster for the inner loop / PR checks. Off by
        /// default — the flake gate (`--repeat 10`) and baselines want the
        /// per-scenario isolation of separate boots.
        #[arg(long, default_value_t = false)]
        shared: bool,
    },
    /// Inspect and manage the integration-test baseline
    /// (`.itest-baseline.toml`) and per-run history (`.itest-runs/`).
    /// These are the management verbs that used to be `itest` flags.
    Baseline {
        #[command(subcommand)]
        cmd: BaselineCmd,
    },
    /// Generate a mermaid diagram from a source of truth into
    /// `docs/generated/`. Diagrams render in GitHub markdown in-diff, so the
    /// committed artifacts are reviewable. See `docs/diagrams-design.md`.
    Diagram {
        /// Which diagram to generate.
        target: DiagramTarget,
        /// Verify the committed diagram is up to date instead of rewriting it;
        /// exit non-zero on drift. For the generated-diagram gate. Not
        /// applicable to `caps` (a runtime snapshot, not a contract).
        #[arg(long, default_value_t = false)]
        check: bool,
        /// (`caps` only) Runtime workload to boot under snemu for the capture;
        /// omit for the default `init` boot.
        #[arg(long)]
        workload: Option<String>,
        /// (`caps` only) snemu instruction-step budget for the capture boot.
        #[arg(long, default_value_t = 150_000_000)]
        steps: u64,
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
        /// Run directory under `.itest-runs/` (a timestamp); omit for the latest.
        #[arg(long)]
        run: Option<String>,
        /// Only the capture for this scenario (matches `fail-<scenario>-*`).
        #[arg(long)]
        scenario: Option<String>,
        /// Print only the last N transcript frames.
        #[arg(long)]
        tail: Option<usize>,
        /// Print only frames containing this substring.
        #[arg(long)]
        grep: Option<String>,
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

/// Failure-capture transcript depth for `cargo xtask itest --capture`.
/// Maps to `itest_harness::CaptureLevel`.
#[derive(Clone, Copy, Debug, ValueEnum)]
enum CaptureArg {
    /// Summary record only — no frame transcript.
    Summary,
    /// Summary + the last ~64 frames.
    Tail,
    /// Summary + every non-ContextSwitch frame (default) — the full story
    /// without the scheduler-switch noise.
    Signal,
    /// Summary + every frame, including ContextSwitch (heavy).
    Full,
}

impl std::fmt::Display for CaptureArg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            CaptureArg::Summary => "summary",
            CaptureArg::Tail => "tail",
            CaptureArg::Signal => "signal",
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
            CaptureArg::Signal => itest_harness::CaptureLevel::Signal,
            CaptureArg::Full => itest_harness::CaptureLevel::Full,
        }
    }
}

/// Which diagram `cargo xtask diagram` generates. The set is open — new
/// targets (static projections + telemetry folds) land as variants.
#[derive(Clone, Copy, Debug, ValueEnum)]
enum DiagramTarget {
    /// Workspace crate dependency graph, from `cargo metadata`.
    Deps,
    /// Integration-test scenario/workload matrix, from the `SCENARIOS` registry.
    ItestMatrix,
    /// Capability derivation tree, folded from a snemu boot's `CapEvent` frames.
    Caps,
    /// Span call-graph, folded from a snemu boot's `SpanStart` frames.
    Trace,
    /// Scheduler task-transition graph, from a snemu boot's `ContextSwitch` frames.
    Switches,
    /// Render the hand-drawn diagram docs' mermaid to local PNGs (needs `mmdc`).
    Png,
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

#[derive(Subcommand, Clone, Copy)]
enum StackCmd {
    /// `docker compose up -d` the stack.
    Up,
    /// `docker compose down` the stack.
    Down,
    /// `docker compose logs -f` the stack.
    Logs,
}

/// Baseline / run-history management. Each verb is a distinct, mutually
/// exclusive operation — the reason these are subcommands rather than the
/// mutually-incompatible `itest` flags they grew from.
#[derive(Subcommand)]
enum BaselineCmd {
    /// Print the canonical baseline summary and exit. By default shows
    /// only the `current` entry per scenario.
    Show {
        /// Include each scenario's prior `current` measurements (history).
        #[arg(long, default_value_t = false)]
        include_history: bool,
        /// Restrict to scenarios with at least one recorded failure,
        /// sorted by Wilson-score lower bound (most-confidently-flaky first).
        #[arg(long, default_value_t = false)]
        flakes_only: bool,
        /// Render the `.pending` sidecar instead of the canonical baseline.
        #[arg(long, default_value_t = false)]
        pending: bool,
    },
    /// Promote `.itest-baseline.toml.pending` into the canonical baseline.
    /// Previous canonical `current` per scenario is pushed to `history`;
    /// the partial marker is stripped.
    Promote,
    /// Delete `.itest-baseline.toml.pending` without promoting. Idempotent.
    Discard,
    /// Rebuild the pending baseline from a per-run history directory's
    /// NDJSON (e.g. `.itest-runs/2026-06-08T12-30-15Z`). Use when the
    /// in-process pending write was lost. Refuses to overwrite an existing
    /// pending file.
    Recover {
        /// The run directory to rebuild from.
        run_dir: std::path::PathBuf,
    },
    /// Retroactively adopt a completed run as the canonical baseline.
    /// With no path, adopts the most recent `.itest-runs/<ts>/`. Use after
    /// an `itest --repeat N` you ran without `--update-baseline`. Previous
    /// canonical entries are pushed to history.
    Adopt {
        /// Specific run directory; omit to adopt the most recent.
        run_dir: Option<std::path::PathBuf>,
    },
    /// Prune `.itest-runs/` to the most recent N directories. Per-run
    /// NDJSON, metadata, and captured failure logs in older runs are removed.
    Prune {
        /// Number of run directories to retain. `0` removes everything.
        #[arg(long, default_value_t = 20)]
        keep_last: usize,
    },
    /// Render the canonical baseline as Prometheus textfile-format metrics
    /// for `node_exporter --collector.textfile` scraping. Atomic write.
    Export {
        /// Output path for the `.prom` textfile.
        path: std::path::PathBuf,
    },
    /// Push the canonical baseline live to an OTLP/HTTP metrics receiver.
    /// With no endpoint, targets the bundled stack's Prometheus receiver at
    /// `http://127.0.0.1:9090/api/v1/otlp` (`/v1/metrics` is appended).
    /// Useful in CI / cron / a post-run hook.
    Push {
        /// Receiver root URL; omit for the bundled stack's endpoint.
        endpoint: Option<String>,
    },
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

#[cfg(test)]
mod retired_command_tests {
    use super::Cli;
    use clap::Parser;

    /// The snemu milestone-1 console-out smoke. Superseded by `snemu-itest`
    /// once snemu modelled Sv39 + virtio (M2); the `minimal-boot` kernel
    /// feature it existed to build went with it.
    #[test]
    fn snemu_m1_smoke_is_gone() {
        assert!(Cli::try_parse_from(["xtask", "snemu"]).is_err());
    }

    /// The deletion is scoped: the rest of the snemu family stays. In
    /// particular `snemu-fork` — it forks one boot across *different*
    /// workloads (layout-preserving DTB overwrite), which
    /// `snemu-itest --share-snapshots` does not do: that shares a boot only
    /// among scenarios of the same workload.
    #[test]
    fn the_rest_of_the_snemu_family_still_parses() {
        assert!(Cli::try_parse_from(["xtask", "snemu-boot"]).is_ok());
        assert!(Cli::try_parse_from(["xtask", "snemu-diff"]).is_ok());
        assert!(Cli::try_parse_from(["xtask", "snemu-fork"]).is_ok());
        assert!(Cli::try_parse_from(["xtask", "snemu-itest"]).is_ok());
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

fn main() -> ExitCode {
    scrub_inherited_cargo_env();
    match Cli::parse().cmd {
        Cmd::Build => build(),
        Cmd::SnemuBoot { features, max_steps, frames, workload } => {
            snemu_boot(&features, max_steps, frames, workload.as_deref())
        }
        Cmd::SnemuDiff { steps, qemu_secs, workload, all, limit } => {
            if all {
                snemu_diff::run_all(steps, qemu_secs, limit)
            } else {
                snemu_diff::run(steps, qemu_secs, workload.as_deref())
            }
        }
        Cmd::SnemuFork { steps } => snemu_diff::run_fork(steps),
        Cmd::SnemuItest {
            steps, limit, only, jobs, no_idle_skip, order, opt, native_ops, block_jit, no_reg_cache,
            share_snapshots,
            native_jit,
            tlb,
            speedup,
        } => {
            let jobs = jobs.unwrap_or_else(|| {
                std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get)
            });
            let speed = itest::snemu_audit::SpeedConfig::resolve(
                Some(speedup), native_ops, block_jit, native_jit, tlb, no_idle_skip, no_reg_cache,
            );
            itest::snemu_audit::run(steps, limit, only.as_deref(), jobs, order, opt, share_snapshots, speed)
        }
        Cmd::SnemuProfile { workload, steps, top, release } => {
            snemu_profile::run(workload.as_deref(), steps, top, release)
        }
        Cmd::SnemuBench { workload, steps, runs, taxonomy, baseline, decode_cache, verify_cache } => {
            if verify_cache {
                snemu_bench::run_verify()
            } else if baseline {
                snemu_bench::run_baseline(runs, decode_cache)
            } else if taxonomy {
                snemu_bench::run_taxonomy(runs, decode_cache)
            } else {
                snemu_bench::run(workload.as_deref(), steps, runs, decode_cache)
            }
        }
        Cmd::Boot { features, workload, burst, ramfb, display } => {
            boot(&features, workload.as_deref(), burst, ramfb, display.as_deref())
        }
        Cmd::Measure { workload, seconds, warmup, timebase_hz, burst, markdown } => {
            measure::measure(&workload, seconds, warmup, timebase_hz, burst, markdown)
        }
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
        Cmd::ItestShow { run, scenario, tail, grep } => {
            itest::show(run.as_deref(), scenario.as_deref(), tail, grep.as_deref())
        }
        Cmd::Itest {
            scenario,
            repeat,
            force,
            skip_unit_tests,
            update_baseline,
            fail_fast,
            no_auto_push,
            jobs,
            cpu_jobs,
            profile,
            capture,
            skip,
            tag,
            shared,
        } => {
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
            itest::run(itest::RunConfig {
                name: scenario,
                repeat,
                force,
                update_baseline,
                fail_fast,
                auto_push: !no_auto_push,
                jobs,
                cpu_jobs,
                profile_filter,
                skip,
                tags: tag,
                shared,
            })
        }
        Cmd::Baseline { cmd } => baseline(cmd),
        Cmd::Diagram { target, check, workload, steps } => match target {
            DiagramTarget::Deps => diagram_cmd::deps(check),
            DiagramTarget::ItestMatrix => diagram_cmd::itest_matrix(check),
            DiagramTarget::Caps if check => {
                eprintln!(
                    "diagram caps: a runtime snapshot, not a contract — --check does not apply"
                );
                ExitCode::from(2)
            }
            DiagramTarget::Caps => diagram_cmd::caps(workload.as_deref(), steps),
            DiagramTarget::Trace => diagram_cmd::trace(workload.as_deref(), steps),
            DiagramTarget::Switches => diagram_cmd::switches(workload.as_deref(), steps),
            DiagramTarget::Png => diagram_cmd::png(),
        },
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

fn baseline(cmd: BaselineCmd) -> ExitCode {
    use itest::baseline as bl;
    match cmd {
        BaselineCmd::Show { include_history, flakes_only, pending } => {
            bl::show_baseline(include_history, flakes_only, pending)
        }
        BaselineCmd::Promote => bl::promote_pending(),
        BaselineCmd::Discard => bl::discard_pending(),
        BaselineCmd::Recover { run_dir } => bl::recover_pending(&run_dir),
        BaselineCmd::Adopt { run_dir } => bl::adopt_run(run_dir),
        BaselineCmd::Prune { keep_last } => bl::prune_runs(keep_last),
        BaselineCmd::Export { path } => bl::export_prom(&path),
        BaselineCmd::Push { endpoint } => bl::push_otlp_metrics(endpoint.as_deref()),
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

/// Build the kernel and run it under snemu, inheriting stdio so snemu's UART
/// output (stdout) and its stop reason (stderr) stream straight to the
/// terminal.
fn snemu_boot(
    features: &str,
    max_steps: Option<u64>,
    frames: bool,
    workload: Option<&str>,
) -> ExitCode {
    let mut features_vec: Vec<&str> = if features.is_empty() {
        Vec::new()
    } else {
        features.split(',').collect()
    };
    // A workload needs the runtime registry compiled in; snemu selects it via
    // the DTB bootarg.
    if workload.is_some() && !features_vec.contains(&"itest-workloads") {
        features_vec.push("itest-workloads");
    }
    let status = qemu::build_kernel(&features_vec).expect("failed to invoke cargo");
    if !status.success() {
        eprintln!("snemu-boot: kernel build failed");
        return ExitCode::from(1);
    }

    let mut cmd = Command::new("cargo");
    cmd.args(["run", "-q", "-p", "snemu", "--"]);
    if frames {
        cmd.arg("--frames");
    }
    if let Some(name) = workload {
        cmd.args(["--workload", name]);
    }
    cmd.arg(qemu::KERNEL_BIN);
    if let Some(n) = max_steps {
        cmd.arg(n.to_string());
    }
    if cmd.status().expect("failed to invoke cargo run -p snemu").success() {
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

    let status = qemu::base_command(&chardev_arg, qemu::DEFAULT_RAM_MB)
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

    let mut cmd = qemu::base_command_ex(&chardev_arg, qemu::DEFAULT_RAM_MB, ramfb, display);
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
            "-p",
            "snitchos-abi",
            "--all-targets",
        ])
        .args(extra_args)
        .status()
        .expect("failed to invoke cargo clippy");

    // The kernel and the userspace program only compile for bare-metal
    // riscv. No `--all-targets`: neither has a host-buildable test target.
    let kernel = Command::new("cargo")
        .args([
            "clippy",
            "-p",
            "kernel",
            "-p",
            "snitchos-user",
            "-p",
            "hello",
            "--target",
            qemu::KERNEL_TARGET,
        ])
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
            "-p",
            "hitch",
            "-p",
            "hitch-pod",
            "-p",
            "stitch",
            "--features",
            "protocol/std,stitch/testing",
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
