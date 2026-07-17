use std::process::{Command, ExitCode};

use clap::{Parser, Subcommand, ValueEnum};

mod audit;
mod diagram_cmd;
mod itest;
mod links;
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
    /// Everything that runs the kernel under the snemu emulator: the
    /// meta-loop driver (`boot`), the QEMU differential oracle (`diff`), the
    /// snapshot/fork harness (`fork`), the measurement spine (`bench`), and the
    /// guest instret profiler (`profile`).
    ///
    /// The everyday test command is not in here: it was promoted out to `itest`,
    /// which is snemu-backed by default (see
    /// plans/xtask-surface-consolidation.md, Step 2.1).
    Snemu {
        #[command(subcommand)]
        cmd: SnemuCmd,
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
        /// Scenario to run. Omit to run all.
        ///
        /// The engines read this differently, and an exact name is the safe
        /// form on both: under `--engine qemu` it is an exact name or a
        /// comma-separated list (`a,b,c`); under snemu it is a **substring**
        /// filter (this was `snemu-itest --only`). So `itest sched` runs every
        /// `sched-*` scenario under snemu, and is an unknown-name error under
        /// qemu.
        scenario: Option<String>,
        /// Which engine runs the scenarios. See [`Engine`].
        #[arg(long, value_enum, default_value_t)]
        engine: Engine,
        /// Per-scenario snemu instruction-step budget. Passing scenarios
        /// short-circuit well under this; the budget only bounds failing ones and
        /// the slow OOM/cooperative workloads. 400M recovers the budget-sensitive
        /// scenarios (e.g. `sched-yield-round-trips`). Accepts `K`/`M`/`B`
        /// suffixes, e.g. `400M`, `1.2B`.
        #[arg(long, default_value = "400M", value_parser = magnitude::parse)]
        steps: u64,
        /// Audit only the first N scenarios (faster smoke).
        #[arg(long)]
        limit: Option<usize>,
        /// Disable `wfi` idle-skip (on by default). The A/B baseline: run the
        /// audit both ways and confirm fidelity + per-scenario instret are
        /// identical — idle-skip must change only speed, never telemetry.
        #[arg(long, hide = true)]
        no_idle_skip: bool,
        /// Scenario packing order: `wall` (default — LPT by the previous run's
        /// wall-time, the true optimisation target but noisy), `instret` (LPT by
        /// prior instret — deterministic, reproducible), or `selection` (no packing —
        /// the A/B baseline). The report prints both counterfactuals regardless.
        #[arg(long, value_enum, default_value_t)]
        order: itest::snemu_audit::PackOrder,
        /// Enable the native-op helper (tier-0.5 JIT): fast-path guest memset/memcpy
        /// (execute natively + charge the interpreter-equivalent instret). A/B it —
        /// on vs off must keep the suite green (fidelity), only faster.
        #[arg(long, hide = true)]
        native_ops: bool,
        /// Enable the Tier-2 block JIT (M6): compile + run hot basic blocks. A/B it
        /// against off while ISA coverage expands — on vs off must stay green +
        /// byte-identical guest instret (the oracle), only faster.
        #[arg(long = "jit", hide = true)]
        block_jit: bool,
        /// With `--jit`, disable the block executor's register caching (M6 inc 4) —
        /// the A/B baseline to isolate the caching's wall-time effect.
        #[arg(long, hide = true)]
        no_reg_cache: bool,
        /// Enable the discovered-snapshot-tree collapse (off by default — the A/B
        /// baseline). Observe-only scenarios (empty branch key, learned from a prior
        /// run's persisted keys) of a workload share one forward run instead of each
        /// re-executing the identical deterministic guest; each replays a prefix of
        /// that shared stream truncated to its own budget, so verdicts are identical
        /// to the fork-per-scenario path. See `docs/snemu-itest-snapshot-tree-design.md`.
        #[arg(long, hide = true)]
        share_snapshots: bool,
        /// Enable **Backend B** (native AArch64 codegen) for the block JIT — implies
        /// `--jit`. Host-only (arm64/macos); A/B it against off, which must stay green
        /// + byte-identical guest instret (the oracle), only faster.
        #[arg(long = "native-jit", hide = true)]
        native_jit: bool,
        /// Enable the software **TLB** (Sv39 translation cache). A/B it against off,
        /// which must stay green + byte-identical guest instret (the oracle), only
        /// faster — this is the lever for the memory/translation pole.
        #[arg(long, hide = true)]
        tlb: bool,
        /// Preset speedup bundle: `low` (idle-skip only), `med` (+native-ops +TLB),
        /// `hi` (+block JIT / Backend A — the fastest *portable*, **the default**),
        /// `extra` (+Backend B native codegen — experimental, host-only, currently
        /// slower). Individual `--jit`/`--tlb`/… flags layer on top. Pass
        /// `--speedup low` for the idle-skip-only A/B baseline.
        #[arg(long, value_enum, default_value = "hi")]
        speedup: itest::snemu_audit::SpeedLevel,
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
        /// Number of scenarios to run in parallel; `1` forces sequential.
        ///
        /// **The default differs per engine**, so this is left unset rather than
        /// carrying one default that would be wrong for one of them (resolved at
        /// dispatch):
        ///
        /// - **snemu**: the machine's available parallelism. Scenarios are
        ///   independent (each owns its own machine) and snemu is a pure
        ///   CPU-bound interpreter, so the sweet spot is the physical core count.
        ///   Turn it down to measure scenario packing, or to leave cores free.
        /// - **qemu**: `10` — validated against an empirical A/B at that width
        ///   (every scenario stayed `consistent` against the sequential
        ///   baseline). The QEMU runner also splits Wfi-bounded (parallel at
        ///   `--jobs`) from Cpu-bounded (`--cpu-jobs`, a separate pass after).
        ///   See `plans/legacy/itest-parallel-scenarios.md`.
        #[arg(long, short = 'j', value_parser = clap::value_parser!(u32).range(1..=64))]
        jobs: Option<u32>,
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
        /// Optimization regime — which kernel/userspace build the scenarios run
        /// against. Each level has distinct failure modes, so flicking between them
        /// localizes a bug:
        ///
        /// - `low`: debug, opt-0 everywhere — the faithful correctness floor. The
        ///   whole suite (incl. supervision) is green here, so a failure is a real
        ///   logic bug.
        /// - `mid`: **release kernel** (opt-3) with the embedded userspace pinned to
        ///   opt-1 (the `build.rs` default). Exercises kernel release codegen while
        ///   dodging the userspace opt≥2 UB class. Fast. This is where
        ///   release-codegen-vs-debug divergences surface: a scenario green under
        ///   `low` can still fail here.
        /// - `high`: **release everywhere** — userspace at opt-3 too. Same kernel as
        ///   `mid`, but lifts the userspace pin, so it *surfaces* the userspace opt≥2
        ///   UB (talc OOM-loop / hang) that `mid` sidesteps.
        ///
        /// So `mid` vs `high` isolates *where* a release-only failure lives:
        /// reproducing under `mid` points at kernel codegen; only under `high` points
        /// at userspace.
        ///
        /// **The default differs per engine** (so this is unset here and resolved at
        /// dispatch, like `--jobs`): snemu defaults to `mid` — fast and green, the
        /// everyday gate; qemu defaults to `low`, its long-standing behaviour. Merging
        /// them onto one default would silently change which kernel one engine tests.
        #[arg(long, value_enum)]
        opt: Option<qemu::OptLevel>,
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
        /// Accepts `K`/`M`/`B` suffixes, e.g. `400M`, `1.2B`.
        #[arg(long, default_value = "150M", value_parser = magnitude::parse)]
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

/// The snemu family. These all mean "do a thing under snemu"; as five
/// top-level `snemu-*` verbs they took five slots and read as unrelated
/// siblings.
#[derive(Subcommand)]
enum SnemuCmd {
    /// Build the kernel and run it under snemu, streaming snemu's output. This
    /// is the meta-loop driver: it always rebuilds the real boot path, then
    /// reports where snemu stops.
    Boot {
        /// Cargo features to enable on the kernel build, comma-separated.
        #[arg(long, default_value = "")]
        features: String,
        /// Cap the run at N instruction steps (snemu's default is 50M).
        /// Accepts `K`/`M`/`B` suffixes, e.g. `400M`, `1.2B`.
        #[arg(long, value_parser = magnitude::parse)]
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
    Diff {
        /// snemu instruction-step budget (round-robin splits it across harts).
        /// Accepts `K`/`M`/`B` suffixes, e.g. `400M`, `1.2B`.
        #[arg(long, default_value = "150M", value_parser = magnitude::parse)]
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
        /// Optimization regime for *both* emulators (they must match, or the diff is
        /// meaningless): `low` (debug, default), `mid` (release kernel), `high`
        /// (release everywhere). Use `--opt mid` to diff **release-vs-release** — the
        /// tool for localizing a snemu fidelity gap that only shows on the release
        /// build (`itest --opt mid` fails while `itest --engine qemu --opt mid`
        /// passes).
        #[arg(long, value_enum, default_value_t = qemu::OptLevel::Low)]
        opt: qemu::OptLevel,
    },
    /// Snapshot/fork harness: boot the common prefix once under snemu, then fork
    /// every workload from that snapshot (clone + DTB bootarg patch). snemu-only;
    /// proves boot amortization.
    ///
    /// Distinct from `snemu-itest --share-snapshots`, which shares a boot only
    /// among scenarios of the *same* workload. This forks one boot across
    /// *different* workloads via a layout-preserving DTB overwrite.
    Fork {
        /// Per-workload step budget after the fork. Accepts `K`/`M`/`B`
        /// suffixes, e.g. `400M`, `1.2B`.
        #[arg(long, default_value = "20M", value_parser = magnitude::parse)]
        steps: u64,
    },
    /// Guest instret profiler: boot a workload to the heartbeat checkpoint, then
    /// run under snemu with exact per-PC counting and report the top kernel
    /// functions by instructions retired. Answers *which code* a scenario spends
    /// its cycles in (e.g. a cross-hart spin-wait vs. real work), the per-function
    /// complement to `snemu-itest`'s per-scenario ruler.
    Profile {
        /// Workload to profile (implies the `itest-workloads` build). Omit for the
        /// default `init` boot.
        #[arg(long)]
        workload: Option<String>,
        /// Instructions to run (post-boot) under the profiler. Accepts
        /// `K`/`M`/`B` suffixes, e.g. `400M`, `1.2B`.
        #[arg(long, default_value = "400M", value_parser = magnitude::parse)]
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
    Bench {
        /// Workload to measure (implies the `itest-workloads` build). Omit for
        /// the default `init` boot.
        #[arg(long)]
        workload: Option<String>,
        /// Instruction-step budget per run. Accepts `K`/`M`/`B` suffixes, e.g.
        /// `400M`, `1.2B`.
        #[arg(long, default_value = "50M", value_parser = magnitude::parse)]
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

/// Which engine `cargo xtask itest` runs scenarios under.
///
/// The promotion (plans/xtask-surface-consolidation.md, Step 2.1): snemu is the
/// everyday runner because it is **deterministic** — one run is the gate, where
/// the QEMU suite needed `--repeat 10` to say anything at its flake rate. QEMU
/// keeps a real job, just not the inner-loop one: it is the oracle snemu is
/// checked against (`cargo xtask snemu diff`), and the escape hatch for when a
/// snemu verdict is doubted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
enum Engine {
    /// The snemu emulator: deterministic, fast, in-process. **The default.**
    #[default]
    Snemu,
    /// QEMU: one process per scenario, wall-clock timeouts, flake-prone.
    Qemu,
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

/// Characterisation of the CLI surface — the net the consolidation work
/// (plans/xtask-surface-consolidation.md) renames things across. These assert
/// what the tree accepts *today*; they are not a design statement. When a step
/// deliberately changes the surface, the failing test here is the point, and the
/// step updates it.
#[cfg(test)]
mod mutant_plan_tests {
    use super::{NOT_MUTATED, mutant_plan};
    use crate::itest::{EXTRA_TEST_ARGS, NOT_HOST_TESTED};

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
        let members = crate::itest::workspace_members().expect("cargo metadata");
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
    use super::Cli;
    use clap::{CommandFactory, Parser};

    /// Step 2.1 — the promotion. `itest` is snemu-backed by default: deterministic,
    /// so a single run is the gate. QEMU stays reachable as `--engine qemu` — the
    /// fidelity oracle snemu is checked against — and `snemu-itest` is gone.
    #[test]
    fn itest_is_snemu_backed_with_qemu_behind_an_engine_flag() {
        use super::{Cmd, Engine};

        let cli = Cli::try_parse_from(["xtask", "itest"]).expect("bare itest parses");
        let Cmd::Itest { engine, .. } = cli.cmd else { panic!("expected Itest") };
        assert_eq!(engine, Engine::Snemu, "itest must default to snemu — the promotion");

        let cli = Cli::try_parse_from(["xtask", "itest", "--engine", "qemu"]).expect("parses");
        let Cmd::Itest { engine, .. } = cli.cmd else { panic!("expected Itest") };
        assert_eq!(engine, Engine::Qemu, "QEMU stays reachable");

        // A scenario name works against both engines.
        assert!(Cli::try_parse_from(["xtask", "itest", "boot-reaches-heartbeat"]).is_ok());
        assert!(
            Cli::try_parse_from(["xtask", "itest", "--engine", "qemu", "boot-reaches-heartbeat"])
                .is_ok()
        );

        // The old name is gone; a bogus engine is rejected.
        assert!(Cli::try_parse_from(["xtask", "snemu-itest"]).is_err());
        assert!(Cli::try_parse_from(["xtask", "itest", "--engine", "bochs"]).is_err());
    }

    /// Both engines' flags still parse after the merge — 2.2 gates them per
    /// engine; 2.1 only moves them under one verb.
    #[test]
    fn the_merged_itest_accepts_both_engines_flags() {
        for argv in [
            // snemu-side (was `snemu-itest`)
            ["itest", "--steps", "400M"].as_slice(),
            ["itest", "--limit", "5"].as_slice(),
            ["itest", "--order", "instret"].as_slice(),
            ["itest", "--opt", "high"].as_slice(),
            ["itest", "--speedup", "low"].as_slice(),
            ["itest", "--jit"].as_slice(),
            // qemu-side
            ["itest", "--engine", "qemu", "--repeat", "3"].as_slice(),
            ["itest", "--engine", "qemu", "--capture", "full"].as_slice(),
            ["itest", "--engine", "qemu", "--cpu-jobs", "2"].as_slice(),
            ["itest", "--engine", "qemu", "--profile", "wfi"].as_slice(),
            ["itest", "--engine", "qemu", "--shared"].as_slice(),
            // shared: --jobs is the packing lever on BOTH engines (its default
            // differs per engine, so it is Option, resolved at dispatch).
            ["itest", "--jobs", "4"].as_slice(),
            ["itest", "--engine", "qemu", "--jobs", "4"].as_slice(),
            ["itest", "--tag", "userspace"].as_slice(),
        ] {
            let full: Vec<&str> = std::iter::once("xtask").chain(argv.iter().copied()).collect();
            assert!(Cli::try_parse_from(&full).is_ok(), "should parse: {argv:?}");
        }
    }

    /// clap's own consistency check: duplicate flags, bad defaults, conflicting
    /// short options. Cheap, and it fails at definition time rather than leaving
    /// a broken verb for someone to discover at the terminal.
    #[test]
    fn the_clap_definition_is_internally_consistent() {
        Cli::command().debug_assert();
    }

    /// Every top-level verb, by name, with the minimum argv that should parse.
    /// A rename or accidental removal fails here.
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

    /// The snemu family is one subcommand group: five verbs that all mean "do a
    /// thing under snemu" took five top-level slots and read as unrelated
    /// siblings.
    ///
    /// `snemu-itest` is deliberately NOT in the group — Step 2.1 renames it to
    /// `itest`, and moving it here first would rename it twice.
    #[test]
    fn the_snemu_family_is_a_subcommand_group() {
        for argv in [
            ["snemu", "boot"].as_slice(),
            ["snemu", "boot", "--workload", "smp"].as_slice(),
            ["snemu", "diff"].as_slice(),
            ["snemu", "diff", "--all"].as_slice(),
            ["snemu", "fork"].as_slice(),
            ["snemu", "bench"].as_slice(),
            ["snemu", "profile"].as_slice(),
        ] {
            let full: Vec<&str> = std::iter::once("xtask").chain(argv.iter().copied()).collect();
            assert!(Cli::try_parse_from(&full).is_ok(), "should parse: {argv:?}");
        }
        // The group needs a member, and a bogus one is rejected.
        assert!(Cli::try_parse_from(["xtask", "snemu"]).is_err());
        assert!(Cli::try_parse_from(["xtask", "snemu", "sideways"]).is_err());
        // The old hyphenated names are gone.
        for gone in ["snemu-boot", "snemu-diff", "snemu-fork", "snemu-bench", "snemu-profile"] {
            assert!(Cli::try_parse_from(["xtask", gone]).is_err(), "{gone} should be gone");
        }
        // …but the one being promoted in 2.1 stays where it is.
        assert!(Cli::try_parse_from(["xtask", "itest"]).is_ok());
    }

    /// A verb we never had must not parse — proves the net actually discriminates
    /// rather than accepting everything.
    #[test]
    fn an_unknown_command_is_rejected() {
        assert!(Cli::try_parse_from(["xtask", "not-a-real-command"]).is_err());
    }

    /// `stack`/`baseline`/`diagram` are subcommand groups: the group alone is
    /// incomplete, and a bogus member is rejected.
    #[test]
    fn subcommand_groups_require_a_valid_member() {
        assert!(Cli::try_parse_from(["xtask", "stack"]).is_err());
        assert!(Cli::try_parse_from(["xtask", "stack", "sideways"]).is_err());
        assert!(Cli::try_parse_from(["xtask", "baseline"]).is_err());
        assert!(Cli::try_parse_from(["xtask", "baseline", "promote"]).is_ok());
        assert!(Cli::try_parse_from(["xtask", "diagram", "not-a-diagram"]).is_err());
    }

    /// The flags Phase 2 reshapes: `itest`'s flake surface (moves behind
    /// `--engine qemu`) and `--jobs` (must survive on both engines — it's the
    /// packing-measurement lever, not a QEMU artifact).
    #[test]
    fn itest_flake_flags_parse_today() {
        for argv in [
            ["itest", "--repeat", "3"].as_slice(),
            ["itest", "--fail-fast", "2"].as_slice(),
            ["itest", "--force"].as_slice(),
            ["itest", "--jobs", "4"].as_slice(),
            ["itest", "--cpu-jobs", "2"].as_slice(),
            ["itest", "--profile", "wfi"].as_slice(),
            ["itest", "--capture", "full"].as_slice(),
            ["itest", "--shared"].as_slice(),
            ["itest", "--tag", "userspace"].as_slice(),
        ] {
            let full: Vec<&str> = std::iter::once("xtask").chain(argv.iter().copied()).collect();
            assert!(Cli::try_parse_from(&full).is_ok(), "itest flag should parse: {argv:?}");
        }
    }

    /// Step 1.3: the seven perf levers are **hidden** from the itest help — they
    /// are snemu-development knobs (the oracle A/B), not test-suite knobs, and
    /// someone running the suite shouldn't be reading about a register allocator.
    /// Hidden, not removed: every A/B workflow keeps working unchanged.
    ///
    /// `--speedup` stays visible — one dial, four positions, the documented way
    /// to pick a regime.
    #[test]
    fn the_perf_levers_are_hidden_but_speedup_is_visible() {
        use clap::CommandFactory;
        let itest = Cli::command();
        let itest = itest
            .get_subcommands()
            .find(|c| c.get_name() == "itest")
            .expect("itest exists");
        let hidden: Vec<&str> = [
            "native-ops", "jit", "no-reg-cache", "native-jit", "tlb", "no-idle-skip",
            "share-snapshots",
        ]
        .into_iter()
        .filter(|name| {
            itest.get_arguments().any(|a| a.get_long() == Some(name) && !a.is_hide_set())
        })
        .collect();
        assert!(hidden.is_empty(), "these perf levers are still visible in help: {hidden:?}");

        let speedup_visible = itest
            .get_arguments()
            .any(|a| a.get_long() == Some("speedup") && !a.is_hide_set());
        assert!(speedup_visible, "--speedup must stay visible");
    }

    /// The levers stay *reachable* — hiding is a help-text change, not a removal.
    #[test]
    fn snemu_itest_perf_levers_parse_today() {
        for argv in [
            ["itest", "--speedup", "low"].as_slice(),
            ["itest", "--jit"].as_slice(),
            ["itest", "--native-jit"].as_slice(),
            ["itest", "--tlb"].as_slice(),
            ["itest", "--native-ops"].as_slice(),
            ["itest", "--no-reg-cache"].as_slice(),
            ["itest", "--no-idle-skip"].as_slice(),
            ["itest", "--share-snapshots"].as_slice(),
            ["itest", "--order", "instret"].as_slice(),
            ["itest", "--opt", "high"].as_slice(),
            ["itest", "-j", "4"].as_slice(),
        ] {
            let full: Vec<&str> = std::iter::once("xtask").chain(argv.iter().copied()).collect();
            assert!(Cli::try_parse_from(&full).is_ok(), "snemu-itest flag should parse: {argv:?}");
        }
    }
}

#[cfg(test)]
mod retired_command_tests {
    use super::Cli;
    use clap::Parser;

    // The M1 console-out smoke used to be `xtask snemu`, and this module asserted
    // `["xtask", "snemu"]` was rejected. Step 1.2 took that name for the snemu
    // *group*, so bare `snemu` now errors as "missing subcommand" — the assertion
    // survived but stopped testing its own name. Deleted rather than left to read
    // as coverage it no longer provides.
    //
    // The smoke's real epitaph isn't a CLI fact anyway: it existed to build the
    // `minimal-boot` kernel feature, and that feature is gone from
    // `kernel/Cargo.toml`. `the_snemu_family_is_a_subcommand_group` covers bare
    // `snemu` being rejected.

    /// `itest` no longer runs the host-side checks as a prerequisite, so the
    /// flag that existed to undo that is gone too. The gate composes explicitly:
    /// `cargo xtask test && cargo xtask itest`.
    #[test]
    fn itest_skip_unit_tests_flag_is_gone() {
        assert!(Cli::try_parse_from(["xtask", "itest", "--skip-unit-tests"]).is_err());
    }

    /// Removing the flag must not disturb `itest`'s own arguments.
    #[test]
    fn itest_still_takes_a_scenario_and_its_own_flags() {
        assert!(Cli::try_parse_from(["xtask", "itest", "boot-reaches-heartbeat"]).is_ok());
        assert!(Cli::try_parse_from(["xtask", "itest", "--repeat", "3"]).is_ok());
    }

    /// The deletion is scoped: the rest of the snemu family stays (now under the
    /// `snemu` group — see `the_snemu_family_is_a_subcommand_group`). In
    /// particular `snemu fork` — it forks one boot across *different* workloads
    /// (layout-preserving DTB overwrite), which `snemu-itest --share-snapshots`
    /// does not do: that shares a boot only among scenarios of the same workload.
    #[test]
    fn the_rest_of_the_snemu_family_still_parses() {
        assert!(Cli::try_parse_from(["xtask", "snemu", "boot"]).is_ok());
        assert!(Cli::try_parse_from(["xtask", "snemu", "diff"]).is_ok());
        assert!(Cli::try_parse_from(["xtask", "snemu", "fork"]).is_ok());
        assert!(Cli::try_parse_from(["xtask", "itest"]).is_ok());
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

/// Dispatch the `snemu` subcommand group.
fn run_snemu(cmd: SnemuCmd) -> ExitCode {
    match cmd {
        SnemuCmd::Boot { features, max_steps, frames, workload } => {
            snemu_boot(&features, max_steps, frames, workload.as_deref())
        }
        SnemuCmd::Diff { steps, qemu_secs, workload, all, limit, opt } => {
            if all {
                snemu_diff::run_all(steps, qemu_secs, limit, opt)
            } else {
                snemu_diff::run(steps, qemu_secs, workload.as_deref(), opt)
            }
        }
        SnemuCmd::Fork { steps } => snemu_diff::run_fork(steps),
        SnemuCmd::Profile { workload, steps, top, release } => {
            snemu_profile::run(workload.as_deref(), steps, top, release)
        }
        SnemuCmd::Bench { workload, steps, runs, taxonomy, baseline, decode_cache, verify_cache } => {
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
    }
}

fn main() -> ExitCode {
    scrub_inherited_cargo_env();
    match Cli::parse().cmd {
        Cmd::Build => build(),
        Cmd::Snemu { cmd } => run_snemu(cmd),
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
        Cmd::Test => itest::run_unit_tests(),
        Cmd::Links => links::check(),
        Cmd::ItestShow { run, scenario, tail, grep } => {
            itest::show(run.as_deref(), scenario.as_deref(), tail, grep.as_deref())
        }
        Cmd::Itest {
            scenario,
            engine,
            steps,
            limit,
            no_idle_skip,
            order,
            native_ops,
            block_jit,
            no_reg_cache,
            share_snapshots,
            native_jit,
            tlb,
            speedup,
            repeat,
            force,
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
            opt,
        } => match engine {
            // The default. Deterministic, so one run is the gate.
            Engine::Snemu => {
                let jobs = jobs.map_or_else(
                    || std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get),
                    |j| j as usize,
                );
                let speed = itest::snemu_audit::SpeedConfig::resolve(
                    Some(speedup),
                    native_ops,
                    block_jit,
                    native_jit,
                    tlb,
                    no_idle_skip,
                    no_reg_cache,
                );
                itest::snemu_audit::run(
                    steps,
                    limit,
                    scenario.as_deref(),
                    jobs,
                    order,
                    opt.unwrap_or(qemu::OptLevel::Mid),
                    share_snapshots,
                    speed,
                )
            }
            // The escape hatch: slower and flake-prone, but it's the engine snemu is
            // checked against, so it stays runnable.
            Engine::Qemu => {
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
                    jobs: jobs.unwrap_or(10),
                    cpu_jobs,
                    profile_filter,
                    skip,
                    tags: tag,
                    shared,
                    opt: opt.unwrap_or(qemu::OptLevel::Low),
                })
            }
        },
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
/// `itest::NOT_HOST_TESTED` is the only way out — the same shape the host test gate
/// already uses, for the same reason.
///
/// Per-crate invocation (rather than one `-p a -p b …`) because the feature args a
/// crate needs are its own: `--features stitch/testing` is invalid for any package
/// that doesn't depend on stitch. Same lesson `MUTANT_CRATES` records.
fn run_clippy(extra_args: &[String]) -> ExitCode {
    let members = match itest::workspace_members() {
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
    let host_plan = match itest::unit_test_plan(&names, itest::NOT_HOST_TESTED, itest::EXTRA_TEST_ARGS) {
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
    let riscv_plan = match itest::riscv_only_plan(&names, itest::NOT_HOST_TESTED) {
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
    itest::unit_test_plan(members, &excluded, extra_args)
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
    let members = match itest::workspace_members() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("mutants: {e}");
            return ExitCode::from(1);
        }
    };
    let names: Vec<&str> = members.iter().map(String::as_str).collect();
    let plan = match mutant_plan(&names, itest::NOT_HOST_TESTED, NOT_MUTATED, itest::EXTRA_TEST_ARGS)
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
