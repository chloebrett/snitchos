//! The snemu fidelity audit: run every itest scenario's assertion body against
//! a frame stream produced by **snemu** instead of QEMU, and tabulate how many
//! already pass. It answers the load-bearing question for "can snemu replace
//! QEMU as the itest backend" — the size of the fidelity gap — without rewriting
//! a single scenario.
//!
//! It reuses the exact `fn(&mut View)` assertion bodies the QEMU suite runs
//! ([`scenario_view_fn`](super::scenario_view_fn)); the only substitution is the
//! frame source. snemu boots each distinct `workload` once (to a step budget),
//! its telemetry is decoded, and every scenario in that group replays against it
//! via [`View::replay`](super::harness::View::replay). Replay is instant: the
//! stream is closed up front, so a `wait_for` match returns at once and a miss
//! fails fast — the audit's wall-clock cost is snemu stepping, not budgets.
//!
//! Two fidelity caveats are *expected* failures, not audit bugs, and the report
//! calls them out: scenarios needing console I/O (`send_input` / `wait_for_log`)
//! have no snemu backing, and negative-oracle scenarios (`assert_absent`) read a
//! closed batch stream as a disconnect. Both are real "snemu can't judge this
//! yet" signals.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::process::ExitCode;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use super::harness::View;
use super::schedule;
use super::snapshot_tree::{self, BranchKeyTable};
use super::{SCENARIOS, scenario_view_fn};
use crate::snemu_diff;

/// Guest instructions stepped between frame-decode checks while recording a shared
/// forward run — mirrors `harness::LIVE_STEP_BATCH` so a recorded frame's instret
/// tag lands on the same batch boundary a live scenario's `wait_for` would observe.
/// Alignment is what makes [`snapshot_tree::frames_within`] truncation reproduce the
/// live frame set exactly.
const SHARED_STEP_BATCH: u64 = 4096;

/// Run one workload's post-boot snapshot forward to `depth` guest instructions,
/// recording every telemetry frame tagged with the instret at which it became
/// decodable. This is the **shared forward run** the zero-input collapse pays once
/// per workload; each observe-only scenario then replays a budget-truncated prefix
/// of it instead of re-executing the identical deterministic guest. Stops early if
/// the guest faults (draining any last decodable frames first).
fn record_shared_stream(
    mut machine: snemu::machine::Machine,
    depth: u64,
) -> Vec<(u64, protocol::stream::OwnedFrame)> {
    use protocol::stream::{OwnedFrame, try_decode_frame};
    let mut consumed = 0usize;
    let mut out = Vec::new();
    loop {
        // Drain every frame already in the TX buffer, tagging each with the current
        // instret (the batch boundary at which it was decoded — see SHARED_STEP_BATCH).
        loop {
            let decoded = {
                let tx = machine.virtio_tx_output();
                try_decode_frame(&tx[consumed..])
                    .ok()
                    .map(|(frame, n)| (OwnedFrame::from_borrowed(&frame), n))
            };
            match decoded {
                Some((frame, n)) => {
                    consumed += n;
                    out.push((machine.instret(), frame));
                }
                None => break,
            }
        }
        if machine.instret() >= depth {
            return out;
        }
        let target = (machine.instret() + SHARED_STEP_BATCH).min(depth);
        while machine.instret() < target {
            if machine.step().is_err() {
                // Faulted: one final decode pass for any frames the trap emitted,
                // then stop — no further frames will come.
                loop {
                    let decoded = {
                        let tx = machine.virtio_tx_output();
                        try_decode_frame(&tx[consumed..])
                            .ok()
                            .map(|(frame, n)| (OwnedFrame::from_borrowed(&frame), n))
                    };
                    match decoded {
                        Some((frame, n)) => {
                            consumed += n;
                            out.push((machine.instret(), frame));
                        }
                        None => return out,
                    }
                }
            }
        }
    }
}

/// One node of the audit pipeline's dependency graph (increment 3), scheduled by
/// [`schedule::run_scheduled`]. Per workload: `Boot` → {`Shared` stream, `Fork`
/// nodes} → `Scenario`s. Producers write their output into a shared store keyed by
/// workload; the precedence guarantees a consumer's inputs exist by the time it runs.
enum PipelineTask<'a> {
    /// Boot this workload to the checkpoint → its post-boot snapshot.
    Boot(Option<&'a str>),
    /// Record this workload's shared forward stream to the given depth (collapse).
    Shared(Option<&'a str>, u64),
    /// Materialise this workload's pre-injection fork nodes at the given instrets.
    Fork(Option<&'a str>, Vec<u64>),
    /// Judge this scenario (original index + descriptor).
    Scenario(usize, &'a itest_harness::Scenario),
}

/// Rough, uniform bottom-level weight for a boot task (guest instret). Boots are
/// roughly equal, so this cancels in relative ordering — the scenario subtree beneath
/// each boot is what actually distinguishes their priorities.
const BOOT_WEIGHT: f64 = 5_000_000.0;

/// The shared post-boot snapshot store: workload → `Arc<Result<Machine>>`. `Arc` so a
/// consumer clones the handle under the lock, then deep-copies the machine *outside*
/// it (the deep copy is the expensive part). `Err` marks a workload that never
/// reached the checkpoint.
type SnapStore<'a> = Mutex<
    std::collections::HashMap<Option<&'a str>, std::sync::Arc<Result<snemu::machine::Machine, String>>>,
>;

/// The shared collapse-stream store: workload → `Arc` of its recorded
/// `(emit_instret, frame)` forward run, read by that workload's collapsed scenarios.
type StreamStore<'a> = Mutex<
    std::collections::HashMap<Option<&'a str>, std::sync::Arc<Vec<(u64, protocol::stream::OwnedFrame)>>>,
>;

/// The shared fork-node store: workload → `Arc` of `first_injection_instret →
/// (pre-injection Machine, its state hash)`, read by that workload's interactive
/// scenarios.
type ForkStore<'a> = Mutex<
    std::collections::HashMap<Option<&'a str>, std::sync::Arc<std::collections::HashMap<u64, (snemu::machine::Machine, u64)>>>,
>;

/// Read a workload's post-boot snapshot from the store and deep-clone a fresh machine
/// to run, or `None` if it never booted. The store lock is held only to clone the
/// `Arc`; the machine copy happens after it's released.
fn read_snapshot<'a>(store: &SnapStore<'a>, workload: Option<&'a str>) -> Option<snemu::machine::Machine> {
    let arc = store.lock().expect("snapshot store").get(&workload).cloned();
    match arc.as_deref() {
        Some(Ok(machine)) => Some(machine.clone()),
        _ => None,
    }
}

/// Memoised **bottom-level** of node `i`: its own weight plus the longest
/// estimated-instret chain of successors beneath it. This is the list-scheduling
/// priority — the deepest root-to-leaf chain (the makespan floor) launches first.
fn bottom_level(i: usize, weights: &[f64], successors: &[Vec<usize>], memo: &mut [f64]) -> f64 {
    if !memo[i].is_nan() {
        return memo[i];
    }
    let mut best = 0.0f64;
    for &j in &successors[i] {
        best = best.max(bottom_level(j, weights, successors, memo));
    }
    let value = weights[i] + best;
    memo[i] = value;
    value
}

/// Push one timeline span onto the shared segment list (a `boot`/`shared`/`fork`
/// setup span or a `scenario` span), for the packing counterfactual + `viz/` renderer.
#[allow(clippy::too_many_arguments, reason = "a flat timeline record; a struct here would just be unpacked at the call")]
fn push_segment(
    segments: &Mutex<Vec<Segment>>,
    kind: &'static str,
    name: &str,
    workload: Option<&str>,
    worker: usize,
    start_s: f64,
    end_s: f64,
    instret: u64,
    pass: bool,
) {
    segments.lock().expect("segments mutex").push(Segment {
        kind,
        name: name.to_owned(),
        workload: workload.map(str::to_owned),
        worker,
        start_s,
        end_s,
        instret,
        pass,
    });
}

/// Advance a workload's post-boot snapshot forward to exactly `target` guest
/// instructions **without injecting any input**, returning the machine at that
/// state — the shared *pre-injection* fork node. Every interactive scenario whose
/// first injection lands at `target` coincides here (identical deterministic guest,
/// no input yet), so this node is materialised once and cloned per child instead of
/// each re-running boot→`target`. Stepping one instruction at a time lands exactly on
/// `target` under the interpreter; the block JIT can overshoot to the next block
/// boundary (a known open question — the A/B oracle gates it). Stops early on a fault.
fn advance_machine_to(mut machine: snemu::machine::Machine, target: u64) -> snemu::machine::Machine {
    while machine.instret() < target {
        if machine.step().is_err() {
            break;
        }
    }
    machine
}

/// Replay a collapsed (observe-only) scenario against the shared stream, truncated
/// to its own budget so its verdict — positive *or* negative-oracle — matches a live
/// run exactly. Returns the same `(Outcome, instret, ram)` shape as [`run_scenario`];
/// a collapsed scenario does no stepping, so its guest cost is zero (the work was
/// paid once in the shared run).
fn run_collapsed(
    stream: &[(u64, protocol::stream::OwnedFrame)],
    scenario: &itest_harness::Scenario,
    max_steps: u64,
) -> Outcome {
    let frames = snapshot_tree::frames_within(stream, budget_for(scenario.name, max_steps));
    let mut view = View::replay(frames);
    match scenario_view_fn(scenario.name)(&mut view) {
        Ok(()) => Outcome::Pass,
        Err(why) => Outcome::Fail { why, console: String::new() },
    }
}

/// One scenario's audit row: its outcome plus the deterministic guest-instruction
/// cost of reaching it. `instret` (guest instructions retired) is what the
/// slowest-scenario table sorts by — contention-free and engine-independent (unlike
/// wall-clock or host step-calls), so it pinpoints where the CPU goes.
struct Row {
    name: &'static str,
    outcome: Outcome,
    instret: u64,
    /// Wall-clock seconds this scenario took — the LPT order predictor by default
    /// (the true optimisation target), with `instret` the deterministic alternative.
    wall_s: f64,
    /// Peak guest RAM footprint (bytes) vs. the machine RAM (MiB) it was allocated,
    /// for the right-sizing report, keyed by workload (RAM is set per workload).
    ram_used_bytes: u64,
    ram_alloc_mb: u32,
    workload: Option<&'static str>,
    /// This scenario's branch key, observed if it ran live (its console
    /// injections), or the prior key carried through if it ran collapsed. Persisted
    /// so a later run can classify it for the zero-input collapse.
    branch_key: snapshot_tree::BranchKey,
    /// Ran against a shared forward run (`--share-snapshots`) rather than stepping
    /// its own machine. A collapsed row has zero measured guest cost, so it's kept
    /// out of the durations cache (its prior instret — the shared-run depth input —
    /// must not be overwritten with 0).
    collapsed: bool,
}

/// One scenario's outcome under the live snemu run.
enum Outcome {
    Pass,
    /// The assertion message, plus the guest's console (UART) output at the point
    /// of failure — so an interactive failure explains itself (what the REPL
    /// printed, an error, a refused command) without a manual re-run.
    Fail { why: String, console: String },
}

/// The machine-readable packing snapshot the audit writes after each run — a single
/// structured JSON object, mirroring the itest history's `*.capture.json`
/// convention (format-follows-shape: TOML for human config, NDJSON for streams,
/// JSON for a tool-consumed snapshot). It's the data layer the `viz/` renderer
/// reads to animate how the workers packed the run. Schema:
/// `docs/snemu-itest-packing-viz-design.md`.
#[derive(serde::Serialize)]
struct PackingReport {
    /// `"LPT"` or `"selection-order"` — which packing produced this timeline.
    packing: &'static str,
    jobs: usize,
    makespan_s: f64,
    /// Sum of every scenario's fork-forward instret plus the one-time boot-once cost.
    total_instret: u64,
    boot_instret: u64,
    workers: Vec<WorkerStat>,
    /// Every boot + scenario span on the timeline, in completion order.
    segments: Vec<Segment>,
}

/// One host worker's share of the run — its busy time and utilization over the
/// makespan (100% = the bottleneck; a low value means a stranded core).
#[derive(serde::Serialize)]
struct WorkerStat {
    id: usize,
    busy_s: f64,
    util: f64,
}

/// One span on a worker's timeline: a per-workload boot, or a scenario fork+run.
/// `start_s`/`end_s` are offsets from the run start, so the renderer can place bars
/// directly.
#[derive(serde::Serialize)]
struct Segment {
    /// `"boot"` (once-per-workload snapshot boot) or `"scenario"` (fork + assert).
    kind: &'static str,
    /// Scenario name, or the workload name for a boot span.
    name: String,
    workload: Option<String>,
    worker: usize,
    start_s: f64,
    end_s: f64,
    /// Guest instructions: `steps` for a scenario, the boot-step count for a boot.
    instret: u64,
    /// Scenario verdict; always `true` for a boot span.
    pass: bool,
}

/// How scenarios are ordered onto the worker queue.
#[derive(Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum PackOrder {
    /// LPT by the previous run's wall-time — the true optimisation target, but noisy
    /// run-to-run. The default.
    #[default]
    Wall,
    /// LPT by the previous run's instret — deterministic, reproducible ranking.
    Instret,
    /// Selection order — no packing; the A/B baseline for the packing win.
    Selection,
}

impl PackOrder {
    fn label(self) -> &'static str {
        match self {
            PackOrder::Wall => "LPT-by-wall",
            PackOrder::Instret => "LPT-by-instret",
            PackOrder::Selection => "selection-order",
        }
    }
}

/// A `--speedup` preset — a named bundle of the snemu optimisation toggles, so the
/// common cases don't need six flags. `low`→`med`→`hi` are **monotonic in speed and
/// portable** (each the previous plus a proven-faster, browser-safe knob); `extra`
/// adds the experimental, host-only bits on top. Individual `--jit`/`--tlb`/… flags
/// still layer on (see [`SpeedConfig::resolve`]).
#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum SpeedLevel {
    /// The fidelity floor: idle-skip only.
    Low,
    /// Low + the memory speedups: native memops + the software TLB. Still interpreted.
    Med,
    /// Med + the Tier-2 block JIT (**Backend A**) — the fastest *portable* config.
    Hi,
    /// Hi + **experimental, non-portable** extras: **Backend B** native AArch64
    /// codegen. Host-only, and currently measured *slower* than Hi (its compile cost
    /// isn't amortised yet) — this tier is for exercising the bleeding edge, not speed.
    Extra,
}

/// The snemu optimisation toggles, bundled — every knob that changes only speed, not
/// behaviour (each on↔off byte-identical by its own oracle). Threaded as one value
/// instead of six bools; applied to a machine by [`apply`](Self::apply).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpeedConfig {
    pub idle_skip: bool,
    pub native_ops: bool,
    pub block_jit: bool,
    pub reg_cache: bool,
    pub native_jit: bool,
    pub tlb: bool,
}

impl SpeedConfig {
    /// The preset for a `--speedup` level. Each is the previous plus an explicit delta:
    /// - **Low** — idle-skip only (the fidelity floor).
    /// - **Med** — Low + the memory speedups (native memops + the software TLB).
    /// - **Hi** — Med + the Tier-2 block JIT (**Backend A** — the fastest portable).
    /// - **Extra** — Hi + **Backend B** native codegen (experimental, host-only, and
    ///   currently *slower* than Hi — the bleeding-edge tier, not a speed tier).
    fn preset(level: SpeedLevel) -> Self {
        let low = Self {
            idle_skip: true,
            native_ops: false,
            block_jit: false,
            reg_cache: true,
            native_jit: false,
            tlb: false,
        };
        let med = Self { native_ops: true, tlb: true, ..low };
        let hi = Self { block_jit: true, ..med };
        let extra = Self { native_jit: true, ..hi };
        match level {
            SpeedLevel::Low => low,
            SpeedLevel::Med => med,
            SpeedLevel::Hi => hi,
            SpeedLevel::Extra => extra,
        }
    }

    /// Resolve the effective config: start from the `--speedup` preset (or the `Low`
    /// baseline when unset), then layer the individual flags on top — the `--*`
    /// enables force a knob on, the `--no-*` disables force it off. So `--speedup med
    /// --native-jit` is Med plus Backend B, and a bare `--jit` is the old behaviour.
    /// `--native-jit` implies the block-JIT frontend.
    #[allow(
        clippy::fn_params_excessive_bools,
        reason = "these are the raw CLI override flags being folded into the bundle"
    )]
    pub fn resolve(
        level: Option<SpeedLevel>,
        native_ops: bool,
        block_jit: bool,
        native_jit: bool,
        tlb: bool,
        no_idle_skip: bool,
        no_reg_cache: bool,
    ) -> Self {
        let mut cfg = Self::preset(level.unwrap_or(SpeedLevel::Low));
        cfg.native_ops |= native_ops;
        cfg.block_jit |= block_jit || native_jit;
        cfg.native_jit |= native_jit;
        cfg.tlb |= tlb;
        if no_idle_skip {
            cfg.idle_skip = false;
        }
        if no_reg_cache {
            cfg.reg_cache = false;
        }
        cfg
    }

    /// A compact list of the enabled toggles for the run banner (`none` if all off).
    fn label(self) -> String {
        let mut on = Vec::new();
        for (enabled, name) in [
            (self.idle_skip, "idle-skip"),
            (self.native_ops, "native-ops"),
            (self.tlb, "tlb"),
            (self.block_jit, "jit-A"),
            (self.native_jit, "jit-B"),
            (self.reg_cache && (self.block_jit || self.native_jit), "reg-cache"),
        ] {
            if enabled {
                on.push(name);
            }
        }
        if on.is_empty() { "none".to_owned() } else { on.join(",") }
    }

    /// Apply every toggle to `machine` (the block JIT frontend is on whenever Backend
    /// B is, since B lowers the same blocks).
    fn apply(self, machine: &mut snemu::machine::Machine) {
        machine.set_idle_skip(self.idle_skip);
        machine.set_native_ops(self.native_ops);
        machine.set_block_jit(self.block_jit || self.native_jit);
        machine.set_native_jit(self.native_jit);
        machine.set_tlb(self.tlb);
        machine.set_register_cache(self.reg_cache);
    }
}

/// Run the audit: build the `itest-workloads` kernel, boot each distinct
/// workload under snemu to `max_steps`, replay every scenario against its
/// group's frames, and print a per-scenario + summary report. `limit` caps the
/// number of workload groups (faster smoke). Exit is always `SUCCESS` — the
/// audit *reports* fidelity, it doesn't gate on it.
pub fn run(
    max_steps: u64,
    limit: Option<usize>,
    only: Option<&str>,
    jobs: usize,
    order: PackOrder,
    opt: crate::qemu::OptLevel,
    share_snapshots: bool,
    speed: SpeedConfig,
) -> ExitCode {
    let (kernel, dtb) = match snemu_diff::prepare_profiled(true, opt) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("snemu-itest: {e}");
            return ExitCode::from(1);
        }
    };

    let selected: Vec<&itest_harness::Scenario> = SCENARIOS
        .iter()
        .filter(|s| only.is_none_or(|sub| s.name.contains(sub)))
        .collect();
    let cap = limit.map_or(selected.len(), |l| l.min(selected.len()));
    let work: Vec<&itest_harness::Scenario> = selected.into_iter().take(cap).collect();
    eprintln!(
        "snemu-itest: auditing {cap} of {} scenario(s), live under snemu, \
         up to {max_steps} steps each, {jobs} worker(s), speed[{}]",
        SCENARIOS.len(),
        speed.label(),
    );

    let started = Instant::now();
    let worker_busy: Vec<AtomicU64> = (0..jobs.max(1)).map(|_| AtomicU64::new(0)).collect();
    let segments: Mutex<Vec<Segment>> = Mutex::new(Vec::new());

    // Phase 1 — boot-once. Boot each *distinct* workload one time to the `entering
    // heartbeat` checkpoint and snapshot it (`Machine: Clone` — the deep copy the
    // machine was built for), so each scenario forks the snapshot instead of
    // re-running the ~25M-instruction boot (~2B saved over the suite, fidelity-
    // exact: the clone carries the emitted-frame history in its virtio-TX buffer).
    // `Machine` is `Sync` (Mutex-backed UART), so the snapshot map is *shared*
    // across workers in phase 2 — the key to scenario-level parallelism below.
    let mut distinct: Vec<Option<&str>> = Vec::new();
    for s in &work {
        if !distinct.contains(&s.workload) {
            distinct.push(s.workload);
        }
    }
    // Ordering key retained for the packing counterfactual + progress display; the
    // scheduler itself orders by bottom-level priority below.
    let history = load_durations();
    let mut sched: Vec<(usize, &itest_harness::Scenario)> = work.iter().copied().enumerate().collect();
    if order != PackOrder::Selection {
        let key = |c: &PackCost| if order == PackOrder::Instret { c.0 } else { c.1 };
        let heaviest = history.values().map(key).max().unwrap_or(u64::MAX);
        sched.sort_by_key(|(_, s)| std::cmp::Reverse(history.get(s.name).map_or(heaviest, &key)));
    }

    // Zero-input collapse + fork-node sharing classification (`--share-snapshots`, off
    // by default). Boot-independent predicates over the *prior* run's persisted keys;
    // the runtime dispatch re-checks that the boot actually succeeded.
    let prior_keys = if share_snapshots { load_prior_keys(&kernel) } else { BranchKeyTable::new() };
    let would_collapse = |name: &str| {
        share_snapshots && prior_keys.get(name).is_some_and(snapshot_tree::BranchKey::is_observe_only)
    };
    let fork_instret = |name: &str| -> Option<u64> {
        share_snapshots
            .then(|| prior_keys.get(name))
            .flatten()
            .and_then(snapshot_tree::BranchKey::first_injection_instret)
    };
    // Per-workload shared-run depth (deepest observer's prior instret; budget cap
    // fallback) and fork instrets — both from the cache, no boot needed.
    let mut depths: std::collections::HashMap<Option<&str>, u64> = std::collections::HashMap::new();
    let mut fork_points: std::collections::HashMap<Option<&str>, std::collections::BTreeSet<u64>> =
        std::collections::HashMap::new();
    for (_, s) in &sched {
        if would_collapse(s.name) {
            let need = history.get(s.name).map_or_else(|| budget_for(s.name, max_steps), |c| c.0);
            let slot = depths.entry(s.workload).or_insert(0);
            *slot = (*slot).max(need);
        }
        if let Some(inst) = fork_instret(s.name) {
            fork_points.entry(s.workload).or_default().insert(inst);
        }
    }

    // Build the pipeline task graph (increment 3). The audit is a forest of
    // per-workload pipelines — boot → {shared stream, fork nodes} → scenarios — and
    // running those as four global barriers strands workers while the slowest
    // workload boots. Instead, schedule the whole graph with critical-path
    // (bottom-level) priorities: as each workload boots, its scenarios become ready
    // and fan out while other workloads are still booting.
    let mut kinds: Vec<PipelineTask> = Vec::new();
    let mut deps_of: Vec<Vec<usize>> = Vec::new();
    let mut weights: Vec<f64> = Vec::new();
    let mut boot_idx: std::collections::HashMap<Option<&str>, usize> = std::collections::HashMap::new();
    for &workload in &distinct {
        boot_idx.insert(workload, kinds.len());
        kinds.push(PipelineTask::Boot(workload));
        deps_of.push(Vec::new());
        weights.push(BOOT_WEIGHT);
    }
    let mut shared_idx: std::collections::HashMap<Option<&str>, usize> = std::collections::HashMap::new();
    for (&workload, &depth) in &depths {
        shared_idx.insert(workload, kinds.len());
        kinds.push(PipelineTask::Shared(workload, depth));
        deps_of.push(vec![boot_idx[&workload]]);
        weights.push(depth as f64);
    }
    let mut fork_idx: std::collections::HashMap<Option<&str>, usize> = std::collections::HashMap::new();
    for (&workload, insts) in &fork_points {
        let list: Vec<u64> = insts.iter().copied().collect();
        let weight = list.last().copied().unwrap_or(0) as f64;
        fork_idx.insert(workload, kinds.len());
        kinds.push(PipelineTask::Fork(workload, list));
        deps_of.push(vec![boot_idx[&workload]]);
        weights.push(weight);
    }
    for &(index, s) in &sched {
        let dep = if would_collapse(s.name) {
            shared_idx[&s.workload]
        } else if fork_instret(s.name).is_some() {
            fork_idx[&s.workload]
        } else {
            boot_idx[&s.workload]
        };
        // Unknown scenario ⇒ heaviest, so a first-seen slow one launches early (as LPT does).
        let weight = history.get(s.name).map_or(u64::MAX, |c| c.0) as f64;
        kinds.push(PipelineTask::Scenario(index, s));
        deps_of.push(vec![dep]);
        weights.push(weight);
    }

    // Bottom-level priority = the node's weight plus the longest chain of successors
    // beneath it (memoised over the reverse edges) — Hu's level, weighted by instret.
    let node_count = kinds.len();
    let mut successors: Vec<Vec<usize>> = vec![Vec::new(); node_count];
    for (i, ds) in deps_of.iter().enumerate() {
        for &d in ds {
            successors[d].push(i);
        }
    }
    let mut bottom = vec![f64::NAN; node_count];
    for i in 0..node_count {
        bottom_level(i, &weights, &successors, &mut bottom);
    }
    let nodes: Vec<schedule::Node> = (0..node_count)
        .map(|i| schedule::Node { deps: deps_of[i].clone(), priority: bottom[i] })
        .collect();

    // Shared stores the tasks read their deps' outputs from and write their own into;
    // precedence guarantees a consumer's inputs exist by the time it runs.
    let snap_store: SnapStore = Mutex::new(std::collections::HashMap::new());
    let stream_store: StreamStore = Mutex::new(std::collections::HashMap::new());
    let fork_store: ForkStore = Mutex::new(std::collections::HashMap::new());
    let rows: Mutex<Vec<Option<Row>>> = Mutex::new((0..work.len()).map(|_| None).collect());
    let boot_instret_acc = AtomicU64::new(0);
    let done = AtomicUsize::new(0);
    let fell_back = AtomicUsize::new(0);
    let unverified_shares = AtomicUsize::new(0);

    // Run a scenario live against a real machine — preferring its verified fork node
    // (interactive prefix share), else the post-boot snapshot, else a fresh boot for a
    // workload that never reached the checkpoint.
    let run_live = |s: &itest_harness::Scenario| -> (Outcome, u64, u64, snapshot_tree::BranchKey) {
        let expected = prior_keys.get(s.name).and_then(snapshot_tree::BranchKey::fork_state_hash);
        let forked = fork_instret(s.name).and_then(|inst| {
            let arc = fork_store.lock().expect("fork store").get(&s.workload).cloned();
            arc.and_then(|nodes| nodes.get(&inst).map(|(m, h)| (m.clone(), *h)))
        });
        let forked = forked.and_then(|(machine, node_hash)| match expected {
            // Fork the node only if its content hash matches the recorded fork-point
            // state — the state-hash verification that makes the share sound.
            Some(exp) if exp == node_hash => Some(machine),
            Some(_) => {
                unverified_shares.fetch_add(1, Ordering::Relaxed);
                None
            }
            None => None,
        });
        let machine = match forked {
            Some(m) => Some(m),
            None => read_snapshot(&snap_store, s.workload).or_else(|| {
                match snemu_diff::load_workload_machine(&kernel, &dtb, s.workload) {
                    Ok(mut m) => {
                        speed.apply(&mut m);
                        Some(m)
                    }
                    Err(_) => None,
                }
            }),
        };
        match machine {
            Some(m) => run_scenario(m, s, max_steps),
            None => (
                Outcome::Fail { why: "snemu load failed".to_owned(), console: String::new() },
                0,
                0,
                snapshot_tree::BranchKey::default(),
            ),
        }
    };

    let dispatch = |worker: usize, task: usize| {
        let start_s = started.elapsed().as_secs_f64();
        let t0 = Instant::now();
        match &kinds[task] {
            PipelineTask::Boot(workload) => {
                let snapshot =
                    boot_snapshot(&kernel, &dtb, *workload, speed);
                let (machine, boot_instret) = match snapshot {
                    Ok((m, n)) => (Ok(m), n),
                    Err(e) => (Err(e), 0),
                };
                boot_instret_acc.fetch_add(boot_instret, Ordering::Relaxed);
                snap_store
                    .lock()
                    .expect("snapshot store")
                    .insert(*workload, std::sync::Arc::new(machine));
                push_segment(&segments, "boot", workload.unwrap_or("(default)"), *workload, worker, start_s, started.elapsed().as_secs_f64(), boot_instret, true);
            }
            PipelineTask::Shared(workload, depth) => {
                let stream = match read_snapshot(&snap_store, *workload) {
                    Some(m) => record_shared_stream(m, *depth),
                    None => Vec::new(),
                };
                let pass_instret = stream.last().map_or(0, |(i, _)| *i);
                stream_store
                    .lock()
                    .expect("stream store")
                    .insert(*workload, std::sync::Arc::new(stream));
                push_segment(&segments, "shared", workload.unwrap_or("(default)"), *workload, worker, start_s, started.elapsed().as_secs_f64(), pass_instret, true);
            }
            PipelineTask::Fork(workload, instrets) => {
                let mut nodes = std::collections::HashMap::new();
                if let Some(mut machine) = read_snapshot(&snap_store, *workload) {
                    for &inst in instrets {
                        machine = advance_machine_to(machine, inst);
                        let hash = machine.state_hash();
                        nodes.insert(inst, (machine.clone(), hash));
                    }
                }
                fork_store
                    .lock()
                    .expect("fork store")
                    .insert(*workload, std::sync::Arc::new(nodes));
                push_segment(&segments, "fork", workload.unwrap_or("(default)"), *workload, worker, start_s, started.elapsed().as_secs_f64(), instrets.last().copied().unwrap_or(0), true);
            }
            PipelineTask::Scenario(index, s) => {
                let snapshot_ok =
                    matches!(snap_store.lock().expect("snapshot store").get(&s.workload).map(|a| a.is_ok()), Some(true));
                let stream = if would_collapse(s.name) && snapshot_ok {
                    stream_store.lock().expect("stream store").get(&s.workload).cloned()
                } else {
                    None
                };
                let (outcome, instret, ram_used_bytes, branch_key, collapsed) =
                    if let Some(stream) = stream {
                        // Collapsed: replay the budget-truncated shared stream. A
                        // failure is inconclusive (depth hint too short) → fall back to
                        // a live run for the authoritative verdict.
                        match run_collapsed(&stream, s, max_steps) {
                            Outcome::Pass => {
                                let key = prior_keys.get(s.name).cloned().unwrap_or_default();
                                (Outcome::Pass, 0, 0, key, true)
                            }
                            Outcome::Fail { .. } => {
                                fell_back.fetch_add(1, Ordering::Relaxed);
                                let (outcome, instret, ram, key) = run_live(s);
                                (outcome, instret, ram, key, false)
                            }
                        }
                    } else {
                        let (outcome, instret, ram, key) = run_live(s);
                        (outcome, instret, ram, key, false)
                    };
                let wall = t0.elapsed().as_secs_f64();
                let pass = matches!(outcome, Outcome::Pass);
                push_segment(&segments, "scenario", s.name, s.workload, worker, start_s, started.elapsed().as_secs_f64(), instret, pass);
                let n = done.fetch_add(1, Ordering::SeqCst) + 1;
                eprintln!(
                    "snemu-itest: [{n:>3}/{cap}] w{worker:<2} {:<40} {:<4} {:>6}M {wall:>6.2}s  {:>4}/{}MiB{}",
                    s.name,
                    if pass { "ok" } else { "FAIL" },
                    instret / 1_000_000,
                    ram_used_bytes / (1024 * 1024),
                    snemu_diff::ram_mb_for(s.workload),
                    if collapsed { "  (shared)" } else { "" },
                );
                rows.lock().expect("rows")[*index] = Some(Row {
                    name: s.name,
                    outcome,
                    instret,
                    wall_s: wall,
                    ram_used_bytes,
                    ram_alloc_mb: snemu_diff::ram_mb_for(s.workload),
                    workload: s.workload,
                    branch_key,
                    collapsed,
                });
            }
        }
        worker_busy[worker].fetch_add(t0.elapsed().as_nanos() as u64, Ordering::Relaxed);
    };

    schedule::run_scheduled(&nodes, jobs, dispatch);

    let results: Vec<Row> = rows.into_inner().expect("rows").into_iter().flatten().collect();
    let boot_instret = boot_instret_acc.load(Ordering::Relaxed);
    let makespan = started.elapsed();

    let fallbacks = fell_back.load(Ordering::Relaxed);
    if fallbacks > 0 {
        eprintln!(
            "snemu-itest: {fallbacks} collapsed scenario(s) failed their shared stream and \
             fell back to a live run (depth hint too short — self-healed for next run)"
        );
    }
    let unverified = unverified_shares.load(Ordering::Relaxed);
    if unverified > 0 {
        eprintln!(
            "snemu-itest: ⚠ {unverified} fork-node share(s) FAILED the state-hash check \
             (materialised node ≠ the scenario's recorded fork-point state) — ran unshared. \
             Investigate: a determinism leak or a JIT/idle-skip boundary drift."
        );
    }

    // Persist this run's per-scenario instret as the packing predictor for next
    // time (a full run overwrites; a filtered `--only` run merges, preserving the
    // durations it didn't touch).
    save_durations(&results);

    // Persist the branch keys observed this run so a later `--share-snapshots` run
    // can classify each scenario. Only when sharing is on, so the A/B baseline
    // (sharing off) leaves the cache — and its own verdict — untouched. The keys are
    // stamped with the kernel fingerprint so a kernel change discards them (a stale
    // key could mis-classify or under-run a shared stream).
    if share_snapshots {
        let mut table = prior_keys.clone();
        for r in results.iter().filter(|r| !r.collapsed) {
            table.insert(r.name.to_owned(), r.branch_key.clone());
        }
        save_prior_keys(&kernel, &table);
    }

    // Counterfactual: re-pack this run's measured per-item wall-times across the
    // workers with ideal (LPT) knowledge, to show how much of the makespan is
    // packing slack vs. the irreducible pole — without needing a second run. The
    // two phases (boot-all-workloads, then fork-and-run-scenarios) are separated by
    // a hard barrier, so the *achievable* ideal is each phase's LPT makespan summed;
    // a single merged pool would optimistically overlap phases that never overlap.
    let segs = segments.into_inner().expect("segments mutex");
    // Two counterfactuals, both respecting the boot→scenario phase barrier (each
    // phase's LPT makespan, summed): one ordered by wall-time (the true optimum),
    // one by instret (what the deterministic scheduler can do). Their diff is the
    // ordering cost; actual − instret-ideal is the online/staleness cost.
    let ideal_wall =
        phase_ideal(&segs, "boot", jobs, true) + phase_ideal(&segs, "scenario", jobs, true);
    let ideal_instret =
        phase_ideal(&segs, "boot", jobs, false) + phase_ideal(&segs, "scenario", jobs, false);

    // Emit the machine-readable packing snapshot for the `viz/` renderer.
    let scenario_instret: u64 = results.iter().map(|r| r.instret).sum();
    write_packing_report(
        order,
        jobs,
        makespan,
        scenario_instret + boot_instret,
        boot_instret,
        &worker_busy,
        segs,
    );

    let exit = print_report(&results, boot_instret, makespan.as_secs_f64());
    print_utilization(&worker_busy, makespan, order, ideal_wall, ideal_instret);
    print_ram_sizing(&results);
    exit
}

/// Where the packing snapshot lands — alongside the itest history family, a stable
/// path the `viz/` renderer loads by default. Gitignored (a machine artifact, not
/// a PR-reviewed baseline).
const PACKING_PATH: &str = ".itest-runs/snemu-packing.json";

/// Serialize the run's packing timeline to [`PACKING_PATH`] as pretty JSON
/// (mirroring `capture.json`). Best-effort: a write failure is a warning, never a
/// gate — the audit's job is the fidelity verdict, the viz is a bonus.
fn write_packing_report(
    order: PackOrder,
    jobs: usize,
    makespan: Duration,
    total_instret: u64,
    boot_instret: u64,
    worker_busy: &[AtomicU64],
    mut segments: Vec<Segment>,
) {
    let makespan_s = makespan.as_secs_f64();
    let makespan_ns = makespan.as_nanos().max(1) as f64;
    let workers = worker_busy
        .iter()
        .enumerate()
        .map(|(id, b)| {
            let busy_ns = b.load(Ordering::Relaxed) as f64;
            WorkerStat { id, busy_s: busy_ns / 1e9, util: 100.0 * busy_ns / makespan_ns }
        })
        .collect();
    // Stable order for a readable diff: by worker, then start time.
    segments.sort_by(|a, b| {
        a.worker.cmp(&b.worker).then(a.start_s.total_cmp(&b.start_s))
    });
    let report = PackingReport {
        packing: order.label(),
        jobs,
        makespan_s,
        total_instret,
        boot_instret,
        workers,
        segments,
    };
    match serde_json::to_string_pretty(&report) {
        Ok(json) => {
            if let Some(dir) = std::path::Path::new(PACKING_PATH).parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            if let Err(e) = std::fs::write(PACKING_PATH, json) {
                eprintln!("snemu-itest: could not write {PACKING_PATH}: {e}");
            }
        }
        Err(e) => eprintln!("snemu-itest: could not serialize packing report: {e}"),
    }
}

/// Least-loaded-worker (LPT) simulation over `items`, each a `(sort_key, duration)`:
/// place items **longest-`sort_key`-first** onto the currently-least-loaded worker
/// and take the peak load. The `sort_key` is what a scheduler would *order* by
/// (wall-time = the true optimum; instret = the deterministic proxy the real run
/// uses); the value *packed* is always the measured wall-time `duration`. Feeding
/// the two different keys yields the two counterfactuals whose diff is the
/// ordering cost.
fn counterfactual_makespan(items: &[(f64, f64)], workers: usize) -> f64 {
    let mut sorted: Vec<(f64, f64)> = items.to_vec();
    sorted.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut loads = vec![0.0f64; workers.max(1)];
    for (_, dur) in sorted {
        let least = loads
            .iter_mut()
            .min_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
            .expect("at least one worker");
        *least += dur;
    }
    loads.into_iter().fold(0.0, f64::max)
}

/// One phase's LPT-ideal makespan, ordered either by wall-time (the optimum) or by
/// guest instret (the deterministic proxy). Packs the measured wall-times either way.
fn phase_ideal(segs: &[Segment], kind: &str, workers: usize, by_wall: bool) -> f64 {
    let items: Vec<(f64, f64)> = segs
        .iter()
        .filter(|s| s.kind == kind)
        .map(|s| {
            let dur = s.end_s - s.start_s;
            (if by_wall { dur } else { s.instret as f64 }, dur)
        })
        .collect();
    counterfactual_makespan(&items, workers)
}

/// Per-**workload** right-sizing: RAM is set per workload (`ram_mb_for`), so a
/// machine must fit its *heaviest* scenario. Aggregates each workload's peak guest
/// footprint and flags workloads whose machine is >2× that peak (shrink
/// candidates) or <1.1× it (tight — do not shrink). `used` here is the max over the
/// workload's scenarios; `n` is how many ran.
fn print_ram_sizing(results: &[Row]) {
    let mb = |bytes: u64| bytes as f64 / (1024.0 * 1024.0);
    // workload → (peak used bytes, alloc MiB, scenario count).
    let mut by_workload: std::collections::BTreeMap<Option<&str>, (u64, u32, u32)> =
        std::collections::BTreeMap::new();
    for r in results.iter().filter(|r| r.ram_used_bytes > 0) {
        let e = by_workload.entry(r.workload).or_insert((0, r.ram_alloc_mb, 0));
        e.0 = e.0.max(r.ram_used_bytes);
        e.2 += 1;
    }
    let line = |w: Option<&str>, used: u64, alloc: u32, n: u32, tag: &str| {
        println!(
            "  {:<24} peak {:>5.1} of {:>3} MiB  ({:.1}× · {n} scn)  {tag}",
            w.unwrap_or("(default/init)"),
            mb(used),
            alloc,
            f64::from(alloc) / mb(used).max(1.0),
        );
    };
    let mut over: Vec<_> = by_workload
        .iter()
        .filter(|(_, (used, alloc, _))| f64::from(*alloc) > 2.0 * mb(*used))
        .collect();
    over.sort_by(|a, b| {
        let hr = |(used, alloc, _): &(u64, u32, u32)| f64::from(*alloc) / mb(*used).max(1.0);
        hr(b.1).partial_cmp(&hr(a.1)).unwrap_or(std::cmp::Ordering::Equal)
    });
    let tight: Vec<_> = by_workload
        .iter()
        .filter(|(_, (used, alloc, _))| mb(*used) > 0.9 * f64::from(*alloc))
        .collect();
    if over.is_empty() && tight.is_empty() {
        return;
    }
    println!("\n=== RAM right-sizing (per-workload peak footprint vs allocated machine) ===");
    for (w, (used, alloc, n)) in over {
        line(*w, *used, *alloc, *n, "shrinkable");
    }
    for (w, (used, alloc, n)) in tight {
        line(*w, *used, *alloc, *n, "⚠ do NOT shrink");
    }
}

fn print_utilization(
    worker_busy: &[AtomicU64],
    makespan: Duration,
    order: PackOrder,
    ideal_wall: f64,
    ideal_instret: f64,
) {
    let makespan_ns = makespan.as_nanos().max(1) as f64;
    let utils: Vec<f64> = worker_busy
        .iter()
        .map(|b| 100.0 * b.load(Ordering::Relaxed) as f64 / makespan_ns)
        .collect();
    if utils.is_empty() {
        return;
    }
    println!(
        "\n=== worker utilization ({} packing, {} worker(s), {:.1}s makespan) ===",
        order.label(),
        utils.len(),
        makespan.as_secs_f64(),
    );
    for (w, u) in utils.iter().enumerate() {
        let bar = "█".repeat((u / 5.0).round() as usize);
        println!("  w{w:<2} {u:>5.1}%  {bar}");
    }
    let mean = utils.iter().sum::<f64>() / utils.len() as f64;
    let min = utils.iter().copied().fold(f64::INFINITY, f64::min);
    println!("  mean {mean:.1}%, min {min:.1}%  (higher + tighter = better packing)");
    let actual = makespan.as_secs_f64();
    let ordering_cost = (ideal_instret - ideal_wall).max(0.0);
    let online_cost = (actual - ideal_instret).max(0.0);
    println!(
        "  ideal LPT re-pack: {ideal_wall:.1}s by wall-time · {ideal_instret:.1}s by instret  \
         (actual {actual:.1}s)"
    );
    println!(
        "    gap breakdown: {ordering_cost:.1}s ordering (instret vs wall) + {online_cost:.1}s \
         online/staleness = {:.1}s total to wall-optimal",
        (actual - ideal_wall).max(0.0),
    );
}

/// Where per-scenario instret is cached between runs, to drive LPT packing. Repo
/// root, gitignored; a plain `<instret> <name>` line per scenario.
const DURATIONS_PATH: &str = ".snemu-itest-durations";

/// Where discovered branch keys are cached between runs — a JSON sibling of the
/// packing report. Stamped with the kernel fingerprint (below); a mismatch discards
/// the whole cache, since a kernel change can move any frame's emit instret and thus
/// invalidate every recorded key + depth.
const BRANCH_KEYS_PATH: &str = ".itest-runs/snemu-branch-keys.json";

/// A cheap, run-to-run-stable fingerprint of the kernel image, so the branch-key
/// cache invalidates when the kernel changes. `DefaultHasher` has fixed keys, so it
/// is deterministic across processes on one toolchain — enough for cache keying.
fn kernel_fingerprint(kernel: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    kernel.hash(&mut h);
    h.finish()
}

/// Load the prior run's branch keys, or an empty table if absent, corrupt, or
/// stamped with a different kernel fingerprint (invalidated). An empty table means
/// nothing collapses this run — the pass rediscovers every key.
fn load_prior_keys(kernel: &[u8]) -> BranchKeyTable {
    let Ok(text) = std::fs::read_to_string(BRANCH_KEYS_PATH) else {
        return BranchKeyTable::new();
    };
    let Ok(doc) = serde_json::from_str::<serde_json::Value>(&text) else {
        return BranchKeyTable::new();
    };
    if doc.get("kernel_hash").and_then(serde_json::Value::as_u64) != Some(kernel_fingerprint(kernel)) {
        return BranchKeyTable::new();
    }
    doc.get("keys")
        .map(std::string::ToString::to_string)
        .and_then(|k| snapshot_tree::parse_branch_keys(&k))
        .unwrap_or_default()
}

/// Persist the branch-key table, stamped with the current kernel fingerprint.
/// Best-effort: a write failure just means next run rediscovers, never a hard error.
fn save_prior_keys(kernel: &[u8], table: &BranchKeyTable) {
    let keys: serde_json::Value =
        serde_json::from_str(&snapshot_tree::serialize_branch_keys(table)).unwrap_or_default();
    let doc = serde_json::json!({ "kernel_hash": kernel_fingerprint(kernel), "keys": keys });
    if let Some(dir) = std::path::Path::new(BRANCH_KEYS_PATH).parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    match serde_json::to_string_pretty(&doc) {
        Ok(json) => {
            if let Err(e) = std::fs::write(BRANCH_KEYS_PATH, json) {
                eprintln!("snemu-itest: could not write {BRANCH_KEYS_PATH}: {e}");
            }
        }
        Err(e) => eprintln!("snemu-itest: could not serialize branch keys: {e}"),
    }
}

/// Cached per-scenario packing cost: `(instret, wall_micros)`. Instret is the
/// deterministic ordering key; wall-microseconds is the default (true-optimum) key.
type PackCost = (u64, u64);

/// Load the cached per-scenario costs (name → `(instret, wall_micros)`). Missing or
/// unparsable file → empty map (first run packs in selection order). Tolerates the
/// old single-column `<instret> <name>` format (wall defaults to 0).
fn load_durations() -> HashMap<String, PackCost> {
    let Ok(text) = std::fs::read_to_string(DURATIONS_PATH) else {
        return HashMap::new();
    };
    text.lines()
        .filter_map(|line| {
            let (instret_str, rest) = line.trim().split_once(char::is_whitespace)?;
            let instret: u64 = instret_str.parse().ok()?;
            // New format: `<instret> <wall> <name>` — the second token parses as a
            // number. Old format: `<instret> <name>` — it doesn't, so wall is unknown
            // (0). The name is the remainder, so a name with spaces survives.
            match rest.trim_start().split_once(char::is_whitespace) {
                Some((wall_str, name)) => match wall_str.parse::<u64>() {
                    Ok(wall) => Some((name.trim().to_owned(), (instret, wall))),
                    Err(_) => Some((rest.trim().to_owned(), (instret, 0))),
                },
                None => Some((rest.trim().to_owned(), (instret, 0))),
            }
        })
        .collect()
}

/// Write this run's per-scenario `(instret, wall_micros)` back to the cache, merged
/// over any prior entries (so a `--only` subset run doesn't erase costs for scenarios
/// it skipped). Best-effort: a write failure just means next run packs from stale or
/// partial history, never a hard error.
fn save_durations(results: &[Row]) {
    let mut costs = load_durations();
    // A collapsed scenario did no stepping (instret 0) — its real cost lives in the
    // shared run. Skip it so its *prior* recorded instret survives; that value is the
    // shared-run depth input, and overwriting it with 0 would collapse the depth to
    // nothing next run.
    for r in results.iter().filter(|r| !r.collapsed) {
        // Record every scenario, including the 0-cost ones whose assertion is
        // already satisfied from the forked checkpoint's frame buffer — they're
        // legitimately cheap, and omitting them makes LPT mistake them for
        // unknown-and-therefore-heaviest next run.
        let wall_micros = (r.wall_s * 1_000_000.0) as u64;
        costs.insert(r.name.to_owned(), (r.instret, wall_micros));
    }
    let mut lines: Vec<(String, PackCost)> = costs.into_iter().collect();
    lines.sort_by_key(|(_, (instret, _))| std::cmp::Reverse(*instret));
    let mut body = String::new();
    for (name, (instret, wall)) in &lines {
        let _ = writeln!(body, "{instret} {wall} {name}");
    }
    let _ = std::fs::write(DURATIONS_PATH, body);
}

/// The UART marker for the boot-once checkpoint: `kmain` prints it once, after all
/// boot + per-workload spawn, just before the heartbeat loop. Snapshotting here
/// captures a workload's entire boot; scenarios fork and step forward from it
/// (the first heartbeat and everything after happens post-fork).
const CHECKPOINT: &[u8] = b"entering heartbeat";

/// Budget for booting a snapshot to [`CHECKPOINT`]. A normal boot reaches it in
/// ~25M steps; the generous cap lets heavier-spawn workloads through while failing
/// fast for a workload that crashes before the heartbeat loop (→ fresh-boot
/// fallback).
const CHECKPOINT_BUDGET: u64 = 60_000_000;

/// Boot a fresh machine for `workload` to [`CHECKPOINT`] with idle-skip set — the
/// shared snapshot every scenario on this workload forks. `Err` if it never reached
/// the checkpoint (each scenario then boots fresh).
fn boot_snapshot(
    kernel: &[u8],
    dtb: &[u8],
    workload: Option<&str>,
    speed: SpeedConfig,
) -> Result<(snemu::machine::Machine, u64), String> {
    let mut machine = snemu_diff::load_workload_machine(kernel, dtb, workload)?;
    // Set on the snapshot; the per-scenario forks inherit it through `clone`.
    speed.apply(&mut machine);
    machine.run_until_uart(CHECKPOINT, CHECKPOINT_BUDGET)?;
    // Report the boot-once cost as guest instret (not host step calls) to match the
    // per-scenario metric — a block collapses many instructions into one step.
    let boot_instret = machine.instret();
    Ok((machine, boot_instret))
}

/// Drive `scenario` against `machine` (a fresh or forked live machine), returning
/// its outcome, the guest-instruction cost of reaching it, its peak RAM, and the
/// branch key it produced (the console injections it performed — empty ⇒
/// observe-only, the collapse eligibility a later run reads).
fn run_scenario(
    machine: snemu::machine::Machine,
    scenario: &itest_harness::Scenario,
    max_steps: u64,
) -> (Outcome, u64, u64, snapshot_tree::BranchKey) {
    let mut view = View::live(machine, budget_for(scenario.name, max_steps));
    let outcome = match scenario_view_fn(scenario.name)(&mut view) {
        Ok(()) => Outcome::Pass,
        Err(why) => Outcome::Fail {
            why,
            console: console_tail(view.console_output().unwrap_or_default().as_str()),
        },
    };
    (
        outcome,
        view.guest_instret().unwrap_or(0),
        view.ram_high_water().unwrap_or(0),
        view.branch_key().clone(),
    )
}

/// Per-scenario step-budget override. Most scenarios reach their assertion well
/// under the default; a handful are genuinely **budget-bound** under snemu's
/// instruction clock — their thresholds (N consumed samples, an OOM, a reaper
/// completing) require the scheduler-gated heartbeat to fire many times, which is
/// hundreds of millions to low billions of instructions. Those pass with the
/// budget below (each measured), while the default keeps the fast majority fast.
/// A larger caller `--steps` still wins (`.max`).
fn budget_for(name: &str, default: u64) -> u64 {
    // A *cap*, not a floor: the sole `assert_absent` scenario. A negative oracle
    // scans for the bad frame until its window elapses — and under snemu (batch)
    // "window elapsed" means the step budget is exhausted (see `View::assert_absent`
    // / `Advance::Disconnected`). So this scenario consumes its *entire* budget
    // confirming absence, regardless of the workload: at the 400M default it was the
    // suite's tail pole (~9s) doing nothing but re-scanning idle heartbeats. Its
    // real need is small and measured: `tlb_remap_rounds >= 100` is reached by ~4M
    // post-fork, and the cumulative `tlb_stale_reads` oracle re-emits every
    // heartbeat, so a handful of heartbeats of absence is conclusive. 60M gives
    // ~10+ heartbeats of margin. A larger `--steps` deliberately does NOT raise this
    // one — more budget is pure wasted scanning, not more coverage.
    if name == "smp-tlb-shootdown-visible" {
        return 60_000_000;
    }
    let needed = match name {
        "workload-cooperative-baseline" => 1_000_000_000,
        "frame-allocator-oom" | "heap-oom" => 3_000_000_000,
        "spawn-reclaims-memory" | "spawn-reclaims-names" => 2_500_000_000,
        // The stim editor is a Stitch tree-walker (interpreter-in-interpreter):
        // trivial in release (~8M) but ~407M in the debug kernel, just over the
        // default. Budget it for the debug fidelity run; release finishes far under.
        "stim-edits-a-file-and-saves" => 600_000_000,
        // `stitch-fs-loads-and-runs` no longer needs an override: `primes(5)`
        // (down from `(10)`) reaches its assertion in ~310M instructions, under
        // the default budget.
        _ => return default,
    };
    needed.max(default)
}

/// Print the per-scenario pass/fail lines, the slowest-scenario table (where the
/// CPU goes), and the headline "N/M pass" summary. `boot_instret` is the one-time
/// per-workload snapshot-boot cost, folded into the honest total-instret figure
/// (the per-scenario steps are post-fork only).
fn print_report(results: &[Row], boot_instret: u64, elapsed_secs: f64) -> ExitCode {
    let passed = results.iter().filter(|r| matches!(r.outcome, Outcome::Pass)).count();
    let total = results.len();

    println!("\n=== snemu itest fidelity ===");
    for Row { name, outcome, .. } in results {
        match outcome {
            Outcome::Pass => println!("  PASS  {name}"),
            Outcome::Fail { why, console } => {
                println!("  FAIL  {name}\n          {}", first_line(why));
                // The guest's own words at the moment of failure — the fastest way
                // to see *why* an interactive scenario broke (a REPL error, a
                // refused command) without re-running by hand.
                if !console.is_empty() {
                    println!("          --- console ---");
                    for line in console.lines() {
                        println!("          | {line}");
                    }
                }
            }
        }
    }

    print_slowest(results, boot_instret);

    println!(
        "\n{passed}/{total} scenarios pass under snemu ({:.0}% fidelity, {elapsed_secs:.1}s)",
        if total == 0 { 0.0 } else { 100.0 * passed as f64 / total as f64 },
    );
    ExitCode::SUCCESS
}

/// The scenarios that cost the most guest instructions to judge — the compute
/// tail that floors the parallel wall-clock (no fan-out beats the single slowest
/// one). Sorted by deterministic `steps`, so it's the same ranking every run and
/// the honest target list for "assert the same thing for less CPU".
fn print_slowest(results: &[Row], boot_instret: u64) {
    const TOP: usize = 12;
    let mut ranked: Vec<&Row> = results.iter().filter(|r| r.instret > 0).collect();
    ranked.sort_unstable_by_key(|r| std::cmp::Reverse(r.instret));
    if ranked.is_empty() {
        return;
    }
    // Total = per-scenario fork-forward instructions + the one-time boot-once
    // cost (paid per workload, not per scenario). The scenario steps are post-fork
    // only, so the boot line makes the north-star figure honest.
    let scenario_total: u64 = ranked.iter().map(|r| r.instret).sum();
    let total = scenario_total + boot_instret;
    println!(
        "\n=== slowest by guest instructions ({} total, {} boot-once) ===",
        magnitude::format(total),
        magnitude::format(boot_instret)
    );
    for r in ranked.iter().take(TOP) {
        let pct = if total == 0 { 0.0 } else { 100.0 * r.instret as f64 / total as f64 };
        println!("  {:>8}  {:>4.1}%  {}", magnitude::format(r.instret), pct, r.name);
    }
}

/// A scenario failure message can be multi-line (a dumped frame tail); the
/// report shows only its first line to stay scannable.
fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or(s)
}

/// The last chunk of the guest's console output — where an error or the last
/// prompt lives. Bounded so a chatty program doesn't flood the report.
fn console_tail(output: &str) -> String {
    const MAX_LINES: usize = 20;
    let lines: Vec<&str> = output.lines().collect();
    let start = lines.len().saturating_sub(MAX_LINES);
    lines[start..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::{SpeedConfig, SpeedLevel};

    /// No overrides ⇒ the preset, exactly. The four tiers are cumulative:
    /// Low ⊂ Med ⊂ Hi ⊂ Extra, with `idle_skip`/`reg_cache` on throughout.
    #[test]
    fn a_preset_with_no_overrides_is_the_preset() {
        let low = SpeedConfig::resolve(Some(SpeedLevel::Low), false, false, false, false, false, false);
        assert!(low.idle_skip && low.reg_cache, "Low is the fidelity floor, not a no-op");
        assert!(!low.native_ops && !low.tlb && !low.block_jit && !low.native_jit);

        let med = SpeedConfig::resolve(Some(SpeedLevel::Med), false, false, false, false, false, false);
        assert!(med.native_ops && med.tlb, "Med adds the memory speedups");
        assert!(!med.block_jit);

        let hi = SpeedConfig::resolve(Some(SpeedLevel::Hi), false, false, false, false, false, false);
        assert!(hi.block_jit && hi.native_ops && hi.tlb, "Hi is Med + Backend A");
        assert!(!hi.native_jit);

        let extra =
            SpeedConfig::resolve(Some(SpeedLevel::Extra), false, false, false, false, false, false);
        assert!(extra.native_jit && extra.block_jit, "Extra is Hi + Backend B");
    }

    /// The enable overrides layer *on top of* a preset — they force a knob on, and
    /// never off. `--speedup low --jit` is the Low floor plus the block JIT.
    #[test]
    fn enable_overrides_add_to_a_preset() {
        let cfg = SpeedConfig::resolve(Some(SpeedLevel::Low), true, true, false, true, false, false);
        assert!(cfg.native_ops && cfg.block_jit && cfg.tlb);
        assert!(cfg.idle_skip, "an enable override must not disturb the floor");
    }

    /// `--native-jit` implies the block-JIT frontend: Backend B is a codegen for
    /// blocks, so asking for it without the frontend is meaningless.
    #[test]
    fn native_jit_implies_the_block_jit_frontend() {
        let cfg = SpeedConfig::resolve(Some(SpeedLevel::Low), false, false, true, false, false, false);
        assert!(cfg.native_jit, "asked for Backend B");
        assert!(cfg.block_jit, "Backend B is a block codegen — it needs the frontend");
    }

    /// The two `--no-*` flags are the only way to turn something *off*, and they
    /// beat the preset that turned it on. (There is deliberately no `--no-jit`:
    /// step down a tier instead.)
    #[test]
    fn no_overrides_force_a_knob_off_against_the_preset() {
        let cfg = SpeedConfig::resolve(Some(SpeedLevel::Hi), false, false, false, false, true, true);
        assert!(!cfg.idle_skip, "--no-idle-skip beats the preset");
        assert!(!cfg.reg_cache, "--no-reg-cache beats the preset");
        assert!(cfg.block_jit, "…and disturbs nothing else");
    }

    /// **The regression this pins.** `resolve(None, ..)` falls back to `Low` — so
    /// the default lives in clap's `default_value = "hi"`, not here. That split has
    /// bitten once already: `--speedup` shipped with no clap default, silently fell
    /// back to `Low`, and the 3× JIT lever sat switched off. If a future edit drops
    /// the clap default, this is the behaviour it silently reverts to.
    #[test]
    fn an_unset_level_falls_back_to_low_not_to_the_cli_default() {
        let cfg = SpeedConfig::resolve(None, false, false, false, false, false, false);
        let low = SpeedConfig::resolve(Some(SpeedLevel::Low), false, false, false, false, false, false);
        assert_eq!(cfg, low, "None must mean Low — the CLI default is clap's job");
        assert!(!cfg.block_jit, "the fallback is the slow floor, not `hi`");
    }

    #[test]
    fn packing_report_serializes_to_the_documented_json_schema() {
        use super::{PackingReport, Segment, WorkerStat};
        let report = PackingReport {
            packing: "LPT",
            jobs: 2,
            makespan_s: 40.0,
            total_instret: 1_000_000,
            boot_instret: 50_000,
            workers: vec![
                WorkerStat { id: 0, busy_s: 38.0, util: 95.0 },
                WorkerStat { id: 1, busy_s: 20.0, util: 50.0 },
            ],
            segments: vec![
                Segment {
                    kind: "boot",
                    name: "frame-oom".to_owned(),
                    workload: Some("frame-oom".to_owned()),
                    worker: 0,
                    start_s: 0.0,
                    end_s: 0.8,
                    instret: 50_000,
                    pass: true,
                },
                Segment {
                    kind: "scenario",
                    name: "frame-allocator-oom".to_owned(),
                    workload: Some("frame-oom".to_owned()),
                    worker: 0,
                    start_s: 0.8,
                    end_s: 38.0,
                    instret: 774_000,
                    pass: true,
                },
            ],
        };

        let json = serde_json::to_string_pretty(&report).expect("serializes");
        // Round-trips as valid JSON, and the renderer's load-bearing fields exist
        // with the right shape (the data-layer contract with `viz/`).
        let v: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        assert_eq!(v["packing"], "LPT");
        assert_eq!(v["jobs"], 2);
        assert_eq!(v["workers"][0]["util"], 95.0);
        assert_eq!(v["segments"][0]["kind"], "boot");
        assert_eq!(v["segments"][1]["name"], "frame-allocator-oom");
        assert_eq!(v["segments"][1]["start_s"], 0.8);
        assert_eq!(v["segments"][1]["pass"], true);
    }
}
