use std::process::{Command, ExitCode};

use clap::{Parser, Subcommand, ValueEnum};

// `qemu` moved to the `xtask-qemu` crate (extracted so scenario edits don't
// recompile it). Aliased here so every existing `crate::qemu::…` reference in
// the submodules keeps resolving unchanged.
use xtask_qemu as qemu;
// snemu tooling moved to the `xtask-snemu` crate; re-imported at root so the
// submodules' `crate::snemu_diff::…` references keep resolving unchanged.
use xtask_snemu::{snemu_bench, snemu_diff, snemu_profile};

mod diagram_cmd;
mod itest;

/// Orchestration commands for the `SnitchOS` workspace.
#[derive(Parser)]
#[command(about, version)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
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
        /// Print a line per scenario as it completes (worker, verdict, guest
        /// instret, wall, RAM), plus the full pass/fail roll-call.
        ///
        /// Off by default: the suite is ~3.5s and deterministic, so the default
        /// output is the *answer* — the failures and a count — not a play-by-play.
        /// The per-scenario lines are a holdover from the minutes-long, flake-prone
        /// QEMU suite, where you watched them to see it hadn't wedged. (The build
        /// chatter above is different — that step really is slow, so its progress
        /// stays.)
        #[arg(long, short = 'v')]
        verbose: bool,
        /// Print the analysis tables: slowest scenarios by guest instret, worker
        /// utilization + packing counterfactuals, and per-workload RAM
        /// right-sizing.
        ///
        /// Off by default because they answer *"where does the time go?"*, not
        /// *"did I break anything?"* — they're what you want when tuning packing or
        /// sizing machines, and noise when gating a commit. Orthogonal to
        /// `--verbose`: `-v --stats` is the full pre-quieting output.
        #[arg(long)]
        stats: bool,
        /// Boot every scenario under snemu's **deterministic frame-scramble** — a
        /// fixed permutation that places each guest RAM frame on a non-contiguous
        /// physical frame. This forces the page-straddle access hazard to fire on
        /// *every* boundary-crossing fetch/load instead of only when the guest
        /// allocator happens to fragment, so it's the standing regression guard for
        /// that bug class. Deterministic (no RNG), so still a one-run gate. Snemu
        /// engine only (a snemu-internal storage remap; QEMU is unaffected).
        #[arg(long)]
        scramble: bool,
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
    ///
    /// Lives here (not in lean `xtask`) because the telemetry targets fold snemu
    /// frames and `itest-matrix` reads the `itest::SCENARIOS` registry — both in
    /// this crate. Lean `xtask` forwards `diagram …` here.
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
        /// Run the optimized (`--release`, opt-3) kernel — matches the release
        /// itests and `diff --opt mid` / `profile --release`. The userspace stays
        /// pinned to opt-1 (the `build.rs` default), same as `--opt mid`. Use this
        /// to stream frames from a release-only fidelity gap, not just diff it.
        #[arg(long)]
        release: bool,
    },
    /// Differential oracle: boot the same kernel under snemu and QEMU and
    /// structurally diff their telemetry frame streams (timestamps normalized).
    Diff {
        /// snemu instruction-step budget (round-robin splits it across harts).
        /// Accepts `K`/`M`/`B` suffixes, e.g. `400M`, `1.2B`.
        #[arg(long, default_value = "150M", value_parser = magnitude::parse)]
        steps: u64,
        /// Safety cap (seconds) on the QEMU collection. QEMU normally stops as soon as
        /// it has emitted the same frame count snemu did (matched capture — see
        /// `collect_qemu`); this bounds the wait if QEMU is slower to reach that count
        /// or stalls. Raise it if a diff FAILs with QEMU having hit the cap short of
        /// snemu's count.
        #[arg(long, default_value_t = 10)]
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
        /// Build/opt regime: `low` (debug), `mid` (release kernel + opt-1
        /// userspace), `hi` (opt-2 userspace), `max` (opt-3 userspace). `hi`/`max`
        /// build the userspace UB class on purpose.
        #[arg(long, value_enum, default_value_t = qemu::OptLevel::Mid)]
        opt: qemu::OptLevel,
        /// Split userspace out per-PC (`[user:0x…]`) instead of collapsing it to
        /// one `[userspace]` bucket. Locates a userspace hot-spot (e.g. a
        /// spin-loop) by raw address — objdump the owning program there. Pair
        /// with `--opt hi`/`max` to profile a specific userspace opt level.
        #[arg(long)]
        user_detail: bool,
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
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

    /// Step 2.2 — the QEMU-only flake/baseline flags error under the default
    /// (snemu) engine instead of being silently ignored. `--repeat 10` on a
    /// deterministic engine looks like it flake-tested but ran once.
    #[test]
    fn qemu_only_flags_are_rejected_under_snemu() {
        use super::{Engine, misplaced_qemu_flags};

        // Nothing set → nothing rejected, on either engine.
        assert!(misplaced_qemu_flags(Engine::Snemu, &[]).is_empty());
        assert!(misplaced_qemu_flags(Engine::Qemu, &[("repeat", true)]).is_empty());

        // A QEMU-only flag set under snemu is named as misplaced.
        assert_eq!(
            misplaced_qemu_flags(Engine::Snemu, &[("--repeat", true), ("--capture", false)]),
            vec!["--repeat"],
        );

        // Under QEMU the same flags are fine.
        assert!(
            misplaced_qemu_flags(Engine::Qemu, &[("--repeat", true), ("--profile", true)])
                .is_empty(),
        );

        // All offenders are collected, in order, for one clear message.
        assert_eq!(
            misplaced_qemu_flags(
                Engine::Snemu,
                &[("--repeat", true), ("--force", true), ("--shared", true)],
            ),
            vec!["--repeat", "--force", "--shared"],
        );
    }

    /// The analysis tables answer "where does the time go?", not "did I break
    /// anything?" — so they're behind `--stats`, orthogonal to `--verbose`.
    #[test]
    fn itest_takes_a_stats_flag_orthogonal_to_verbose() {
        use super::Cmd;

        let cli = Cli::try_parse_from(["xtask", "itest"]).expect("parses");
        let Cmd::Itest { stats, verbose, .. } = cli.cmd else { panic!("expected Itest") };
        assert!(!stats, "tables are tuning output, not gate output");
        assert!(!verbose);

        let cli = Cli::try_parse_from(["xtask", "itest", "--stats"]).expect("parses");
        let Cmd::Itest { stats, verbose, .. } = cli.cmd else { panic!("expected Itest") };
        assert!(stats);
        assert!(!verbose, "--stats must not drag in the per-scenario roll-call");

        // The two axes compose: `-v --stats` is the pre-2.1b output.
        let cli = Cli::try_parse_from(["xtask", "itest", "-v", "--stats"]).expect("parses");
        let Cmd::Itest { stats, verbose, .. } = cli.cmd else { panic!("expected Itest") };
        assert!(stats && verbose);
    }

    /// `--scramble` boots every scenario under the deterministic frame-scramble —
    /// the standing regression guard for the page-straddle access hazard. Off by
    /// default (the plain contiguous layout is the one real hardware produces).
    #[test]
    fn itest_takes_a_scramble_flag() {
        use super::Cmd;

        let cli = Cli::try_parse_from(["xtask", "itest"]).expect("parses");
        let Cmd::Itest { scramble, .. } = cli.cmd else { panic!("expected Itest") };
        assert!(!scramble, "contiguous is the default — it's what a real machine does");

        let cli = Cli::try_parse_from(["xtask", "itest", "--scramble"]).expect("parses");
        let Cmd::Itest { scramble, .. } = cli.cmd else { panic!("expected Itest") };
        assert!(scramble, "--scramble forces the non-contiguous layout");
    }

    /// The suite is 3.5s and deterministic, so the default output is the answer —
    /// failures and a count — not a play-by-play. `--verbose` brings back the
    /// per-scenario lines.
    #[test]
    fn itest_takes_a_verbose_flag() {
        use super::Cmd;

        let cli = Cli::try_parse_from(["xtask", "itest"]).expect("parses");
        let Cmd::Itest { verbose, .. } = cli.cmd else { panic!("expected Itest") };
        assert!(!verbose, "quiet is the default — the run is 3.5s, not a progress bar");

        for argv in [["itest", "--verbose"].as_slice(), ["itest", "-v"].as_slice()] {
            let full: Vec<&str> = std::iter::once("xtask").chain(argv.iter().copied()).collect();
            let cli = Cli::try_parse_from(&full).unwrap_or_else(|e| panic!("{argv:?}: {e}"));
            let Cmd::Itest { verbose, .. } = cli.cmd else { panic!("expected Itest") };
            assert!(verbose, "{argv:?} should enable verbose");
        }
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
            ["itest", "--opt", "max"].as_slice(),
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

    /// `snemu boot --release` runs the opt-3 kernel under snemu — the counterpart
    /// to `profile --release` and `diff --opt mid`, so a release-only fidelity gap
    /// can be observed by streaming frames, not just diffed. Default is debug.
    #[test]
    fn snemu_boot_takes_a_release_flag() {
        use super::{Cmd, SnemuCmd};

        let cli = Cli::try_parse_from(["xtask", "snemu", "boot"]).expect("parses");
        let Cmd::Snemu { cmd: SnemuCmd::Boot { release, .. } } = cli.cmd else {
            panic!("expected snemu boot")
        };
        assert!(!release, "debug is the default");

        let cli = Cli::try_parse_from(["xtask", "snemu", "boot", "--release"]).expect("parses");
        let Cmd::Snemu { cmd: SnemuCmd::Boot { release, .. } } = cli.cmd else {
            panic!("expected snemu boot")
        };
        assert!(release, "--release enables the opt-3 kernel");
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
            ["itest", "--opt", "max"].as_slice(),
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

/// Dispatch the `snemu` subcommand group.
fn run_snemu(cmd: SnemuCmd) -> ExitCode {
    match cmd {
        SnemuCmd::Boot { features, max_steps, frames, workload, release } => {
            snemu_boot(&features, max_steps, frames, workload.as_deref(), release)
        }
        SnemuCmd::Diff { steps, qemu_secs, workload, all, limit, opt } => {
            if all {
                snemu_diff::run_all(steps, qemu_secs, limit, opt)
            } else {
                snemu_diff::run(steps, qemu_secs, workload.as_deref(), opt)
            }
        }
        SnemuCmd::Fork { steps } => snemu_diff::run_fork(steps),
        SnemuCmd::Profile { workload, steps, top, opt, user_detail } => {
            snemu_profile::run(workload.as_deref(), steps, top, opt, user_detail)
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

/// The QEMU-only flags among `flags` (each `(name, was_set)`) that were set under
/// the snemu engine — where they do nothing.
///
/// `itest` merges both engines' flags onto one verb, so a flag only the QEMU
/// runner reads would otherwise be silently ignored under the default snemu
/// engine. That is worst for the flake machinery: `--repeat 10` on a
/// deterministic engine looks like a flake sweep but runs once. Return the
/// offenders (in argv order, for one clear message) so the caller can refuse and
/// point at `--engine qemu`. Empty under `Engine::Qemu` — there they're valid.
///
/// Display flags (`--verbose`/`--stats`) are deliberately not in the caller's
/// list: a no-op display flag misleads no one, unlike a no-op `--repeat`.
fn misplaced_qemu_flags<'a>(engine: Engine, flags: &[(&'a str, bool)]) -> Vec<&'a str> {
    match engine {
        Engine::Qemu => Vec::new(),
        Engine::Snemu => flags.iter().filter(|(_, set)| *set).map(|(n, _)| *n).collect(),
    }
}

fn main() -> ExitCode {
    scrub_inherited_cargo_env();
    match Cli::parse().cmd {
        Cmd::Snemu { cmd } => run_snemu(cmd),
        Cmd::ItestShow { run, scenario, tail, grep } => {
            itest::show(run.as_deref(), scenario.as_deref(), tail, grep.as_deref())
        }
        Cmd::Itest {
            scenario,
            engine,
            verbose,
            stats,
            scramble,
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
        } => {
            // Refuse the QEMU-only flake/baseline/partition flags under the snemu
            // engine, where they'd do nothing. `was_set` is true when the flag
            // departs from its default; `--repeat`/`--cpu-jobs`/`--capture` carry
            // meaningful defaults, so compare against them rather than "present".
            let misplaced = misplaced_qemu_flags(
                engine,
                &[
                    ("--repeat", repeat != 1),
                    ("--fail-fast", fail_fast.is_some()),
                    ("--force", force),
                    ("--update-baseline", update_baseline),
                    ("--no-auto-push", no_auto_push),
                    ("--cpu-jobs", cpu_jobs != 1),
                    ("--profile", profile.is_some()),
                    ("--capture", capture != CaptureArg::Signal),
                    ("--shared", shared),
                    ("--tag", !tag.is_empty()),
                    ("--skip", !skip.is_empty()),
                ],
            );
            if !misplaced.is_empty() {
                use clap::CommandFactory;
                Cli::command()
                    .error(
                        clap::error::ErrorKind::ArgumentConflict,
                        format!(
                            "{} only applies under `--engine qemu`; the default snemu \
                             engine is deterministic, so the flake/baseline machinery \
                             is meaningless there. Re-run with `--engine qemu`, or drop \
                             the flag.",
                            misplaced.join(", "),
                        ),
                    )
                    .exit();
            }
            match engine {
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
                    verbose,
                    stats,
                    scramble,
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
            }
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
    }
}

/// The generated-diagram drift check, moved out of lean `xtask test` (which must
/// not link snemu). It runs here in the nextest phase instead — `xtask-itest`'s
/// test binary already links the snemu build the suite compiles, so this adds no
/// snemu build to the lean tool. A stale committed diagram fails the suite.
#[cfg(test)]
mod diagram_drift_tests {
    #[test]
    fn committed_diagrams_are_up_to_date() {
        assert!(
            super::diagram_cmd::check_all() == std::process::ExitCode::SUCCESS,
            "a generated diagram in docs/generated/ is stale — regenerate with \
             `cargo xtask diagram <target>`",
        );
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

/// Build the kernel and run it under snemu, inheriting stdio so snemu's UART
/// output (stdout) and its stop reason (stderr) stream straight to the
/// terminal.
fn snemu_boot(
    features: &str,
    max_steps: Option<u64>,
    frames: bool,
    workload: Option<&str>,
    release: bool,
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
    // `Mid` = opt-3 kernel with the opt-1 userspace pin (same regime the release
    // itests and `diff --opt mid` use); `Low` = debug. `High` (opt-3 userspace)
    // isn't exposed here — that's the deliberate-UB build, reachable via `itest`.
    let opt = if release { qemu::OptLevel::Mid } else { qemu::OptLevel::Low };
    let status = qemu::build_kernel_profiled(&features_vec, opt).expect("failed to invoke cargo");
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
    cmd.arg(qemu::kernel_bin(release));
    if let Some(n) = max_steps {
        cmd.arg(n.to_string());
    }
    if cmd.status().expect("failed to invoke cargo run -p snemu").success() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}
