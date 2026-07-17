//! Generic `--repeat N` runner. Owns the per-iteration loop, scenario
//! invocation, aggregation, baseline I/O, and the printable output —
//! everything that doesn't depend on what's under test.
//!
//! The consumer (xtask, for now) plugs in:
//!
//! - The scenario list (`&[Scenario]`).
//! - The executor (`run_group`): launches the subject and runs a group
//!   of scenarios, returning a `ScenarioReport` each. This is where the
//!   consumer's boot/View/timing/capture lives.
//! - Hooks: `one_shot_build`, `current_commit`. Optional; the runner
//!   no-ops when `None`.
//! - The baseline file path.
//!
//! Nothing in this module knows about QEMU, virtio, or postcard.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use time::OffsetDateTime;

use crate::aggregate::{Aggregator, RunTotals};
use crate::baseline::{Baseline, BaselineFile};
use crate::history::HistoryWriter;
use crate::verdict::{ComparisonRender, DEFAULT_ALPHA, render_comparison, verdict};

/// Host-CPU profile for a scenario. Used by the parallel runner to
/// decide whether the scenario fans out alongside other work
/// (`Wfi`) or runs in a serial pass after the parallel batch
/// (`Cpu`).
///
/// **Wfi**: the kernel spends most of its time in `wfi` waiting on
/// the guest timer. Host CPU is mostly idle; many of these can
/// share a small core budget without timing distortion.
///
/// **Cpu**: the kernel runs real guest work between observable
/// events (allocator scribbling, context-switch loops, storm
/// workloads). Each one wants its own host core; running two
/// `Cpu` scenarios simultaneously can stretch wall-clock past the
/// harness's per-scenario timeout. See
/// `plans/legacy/itest-parallel-scenarios.md`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CpuProfile {
    #[default]
    Wfi,
    Cpu,
}

/// One scenario: a name + a function that returns `Ok(())` on pass or
/// a human-readable error string on failure. `cpu_profile` defaults
/// to `Wfi`; mark CPU-heavy scenarios via `Scenario::cpu_bound`.
#[derive(Debug)]
pub struct Scenario {
    pub name: &'static str,
    pub run: fn() -> Result<(), String>,
    pub cpu_profile: CpuProfile,
    /// Free-form labels for `--tag` selection (e.g. `["userspace"]`,
    /// `["smp", "stress"]`). Defaults to empty; annotate via `tagged`.
    pub tags: &'static [&'static str],
    /// The runtime workload this scenario boots (the `workload=<name>`
    /// bootarg), or `None` for the default demo. This is the **grouping
    /// key** for shared-boot mode: scenarios with the same `workload`
    /// read one boot. Declared on the catalog row via `on_workload`; see
    /// `plans/legacy/itest-shared-boot-mode.md`.
    pub workload: Option<&'static str>,
}

impl Scenario {
    /// New `Wfi`-profile scenario. Const-fn so it composes inside
    /// `const SCENARIOS: &[Scenario]` arrays.
    pub const fn new(name: &'static str, run: fn() -> Result<(), String>) -> Self {
        Self { name, run, cpu_profile: CpuProfile::Wfi, tags: &[], workload: None }
    }

    /// New `Cpu`-profile scenario. For scenarios that run real
    /// guest CPU work between heartbeats — allocator pressure
    /// loops, context-switch storms, etc. Same const-fn property.
    pub const fn cpu_bound(name: &'static str, run: fn() -> Result<(), String>) -> Self {
        Self { name, run, cpu_profile: CpuProfile::Cpu, tags: &[], workload: None }
    }

    /// Annotate with selection tags. Chainable and const so it composes
    /// after `new`/`cpu_bound` inside `const SCENARIOS` arrays:
    /// `Scenario::cpu_bound("spawn-storm", f).tagged(&["smp", "stress"])`.
    #[must_use]
    pub const fn tagged(mut self, tags: &'static [&'static str]) -> Self {
        self.tags = tags;
        self
    }

    /// Declare the runtime workload this scenario boots — the grouping key
    /// for shared-boot mode. Chainable and const, composes after
    /// `tagged`: `Scenario::new("u", f).tagged(&["userspace"]).on_workload("userspace")`.
    #[must_use]
    pub const fn on_workload(mut self, workload: &'static str) -> Self {
        self.workload = Some(workload);
        self
    }
}

/// Select scenarios carrying any of the requested `tags` (set union):
/// `--tag smp --tag stress` (or `--tag smp,stress`) yields every
/// scenario tagged either — same comma-means-also convention as the
/// positional scenario list (`itest a,b,c`). A requested tag carried by
/// *no* scenario is an error (almost always a typo — matching nothing
/// silently would hide it); the returned message names the offending
/// tag. An empty `tags` slice is a no-op that returns every scenario
/// unchanged.
pub fn select_by_tags<'a>(
    scenarios: &[&'a Scenario],
    tags: &[String],
) -> Result<Vec<&'a Scenario>, String> {
    if tags.is_empty() {
        return Ok(scenarios.to_vec());
    }
    if let Some(unknown) = tags
        .iter()
        .find(|t| !scenarios.iter().any(|s| s.tags.contains(&t.as_str())))
    {
        let mut known: Vec<&str> = scenarios.iter().flat_map(|s| s.tags.iter().copied()).collect();
        known.sort_unstable();
        known.dedup();
        return Err(format!(
            "unknown tag: {unknown}\nknown tags: {}",
            known.join(", ")
        ));
    }
    Ok(scenarios
        .iter()
        .copied()
        .filter(|s| s.tags.iter().any(|t| tags.iter().any(|req| req == t)))
        .collect())
}

/// Everything the runner needs to know about one scenario's execution,
/// returned by the consumer's executor (`run_group`). This replaces the
/// old thread-local hooks (`log_path_for` / `max_wait_for` /
/// `capture_for`): instead of the runner scraping thread-locals the
/// consumer's harness stashed during `(s.run)()`, the consumer now *runs*
/// the scenario and hands back a structured report. No action-at-a-
/// distance.
pub struct ScenarioReport {
    /// `Ok(())` on pass, or the scenario's human-readable failure string.
    pub result: Result<(), String>,
    /// `(actual_elapsed, budget)` of the consumer's longest internal wait
    /// (e.g. a virtio-frame wait), for the inline `max wait …` display.
    /// `None` if the consumer measured nothing.
    pub max_wait: Option<(Duration, Duration)>,
    /// The consumer's structured failure capture for cause-bucket
    /// classification, or `None` (e.g. a value-mismatch with no wait).
    pub capture: Option<crate::signature::FailureCapture>,
    /// Path to the scenario's log file, so the runner can dump the tail
    /// on failure and copy it into the run-dir. `None` if no log.
    pub log_path: Option<PathBuf>,
}

/// The consumer's executor: run a group of scenarios and return one
/// report each, in order. The runner groups scenarios (separate mode =
/// singletons; shared mode = by workload) and calls this per group; the
/// consumer decides how to launch the subject and run the assertions.
///
/// `Send + Sync` because the parallel path calls it from worker threads.
/// `Option`/`&dyn` so it's `Copy` — threading it by value into workers is
/// free. `None` falls back to calling `(s.run)()` directly (used by the
/// crate's own tests; real consumers supply an executor).
pub(crate) type RunGroupFn<'a> =
    Option<&'a (dyn Fn(&[&Scenario]) -> Vec<ScenarioReport> + Send + Sync)>;

/// Everything the consumer plugs into a `run()` — the executor plus a
/// handful of optional callbacks and paths. Each `Option<&dyn Fn>` lets
/// consumers supply only what they need. Lifetime parameter matches the
/// surrounding `run()` call — nothing escapes.
#[derive(Default)]
pub struct RunnerConfig<'a> {
    /// Called once before any scenarios. Errors abort the run with
    /// exit code 2. Typical use: `cargo build -p kernel`.
    pub one_shot_build: Option<&'a dyn Fn() -> Result<(), String>>,

    /// The consumer's executor: launch the subject and run a group of
    /// scenarios, returning one `ScenarioReport` each. The runner groups
    /// scenarios and calls this per group; `None` falls back to calling
    /// `(s.run)()` directly with an empty report (the crate's own tests).
    /// This replaced the old `log_path_for` / `max_wait_for` /
    /// `capture_for` thread-local hooks.
    pub run_group: RunGroupFn<'a>,

    /// Called once when writing a new baseline entry. Returns the
    /// short git commit hash, or `None` if unavailable.
    pub current_commit: Option<&'a dyn Fn() -> Option<String>>,

    /// Path to the baseline TOML. If `None`, baseline comparison is
    /// skipped and `--update-baseline` is a no-op (with a warning).
    pub baseline_file: Option<PathBuf>,

    /// Stop early once cumulative failures reach this many. `None`
    /// (the default) lets the run go to completion. Useful for
    /// "confirm flakiness fast" workflows: with K=3, a flaky kernel
    /// usually wraps within ~30 scenario-runs instead of the full
    /// `--repeat N`. The check fires at iteration boundaries, not
    /// mid-iteration — the per-run output stays coherent.
    pub fail_fast: Option<u32>,

    /// Path where a *partial* baseline gets written if the run is
    /// interrupted mid-`--repeat` (graceful Ctrl-C). Only consulted
    /// when `update_baseline` is true; never overwrites the canonical
    /// baseline file. See `plans/legacy/itest-history-and-pending.md`.
    pub pending_baseline: Option<PathBuf>,

    /// Interrupt signal. The runner reads this at every iteration
    /// boundary; when it's `true`, the loop breaks gracefully and
    /// (if `update_baseline`) the partial baseline goes to
    /// `pending_baseline` instead of the canonical file. The caller
    /// sets it from a signal handler (xtask uses `ctrlc`).
    pub interrupt: Option<&'a std::sync::atomic::AtomicBool>,

    /// Root directory for per-run history (NDJSON + metadata + log
    /// copies). When set, the runner creates a timestamped
    /// subdirectory at start, writes `metadata.toml`, and appends one
    /// JSON row per scenario invocation to `iterations.ndjson`. When
    /// `None`, no history is written. See
    /// `plans/legacy/itest-history-and-pending.md` step C.
    pub history_root: Option<PathBuf>,

    /// Number of worker threads for per-iteration scenario fan-out.
    /// `0` and `1` both mean sequential execution (preserves the
    /// pre-parallel output format). `>1` enables the worker pool
    /// (see `plans/legacy/itest-parallel-scenarios.md`). Each iteration
    /// still runs to completion (its own `RunTotals`, fail-fast and
    /// interrupt check) before the next begins — fan-out is intra-
    /// iteration only.
    pub jobs: u32,

    /// Worker count for the Cpu-bound batch (the scenarios marked
    /// `Scenario::cpu_bound`). They run in their own pass after the
    /// Wfi batch finishes. Floored to 1, so the batch runs **serially
    /// by default** — Cpu scenarios run real guest work (often
    /// multi-vcpu) and must not contend with each other. Set `> 1`
    /// explicitly to parallelise. `0` is floored to 1 (the CLI
    /// rejects 0 outright).
    pub cpu_jobs: u32,

    /// The full command line that launched this run, recorded into the
    /// run's `metadata.toml` for exact reproduction and to capture the
    /// parallelism a failure occurred under. `None` omits it.
    pub invocation: Option<String>,

    /// Shared-boot mode. `false` (default) = separate: each scenario is its
    /// own group, so the executor spawns one subject per scenario (today's
    /// flake-gate semantics). `true` = shared: scenarios are grouped by
    /// `workload`, so each group's same-workload scenarios run against one
    /// subject. The executor is identical for both — only the grouping
    /// differs. See `plans/legacy/itest-shared-boot-mode.md`.
    pub shared: bool,
}

/// Run `scenarios` `repeat` times. If `update_baseline` is true, write
/// the run's per-scenario results back to the baseline file.
///
/// Exit code: `2` on build-hook failure; `1` if any scenario failed
/// any iteration; `0` otherwise. Single-iteration runs match the
/// pre-extraction xtask behaviour (no aggregate printed).
pub fn run(
    scenarios: &[&Scenario],
    repeat: u32,
    update_baseline: bool,
    config: &RunnerConfig<'_>,
) -> ExitCode {
    if let Some(build) = config.one_shot_build
        && let Err(e) = build()
    {
        eprintln!("kernel build failed: {e}");
        return ExitCode::from(2);
    }

    let runs = repeat.max(1);
    let mut aggregator = Aggregator::new();
    let mut interrupted = false;

    // History (tier 2/3). Create the run-dir + open the NDJSON writer
    // if a history_root was supplied. Failure here is non-fatal: log
    // and proceed without history.
    //
    // The writer is wrapped in `Mutex<Option<_>>` so the parallel path
    // can share it across worker threads. The lock is held only for
    // the append; serial paths pay essentially no cost.
    let now_for_history = OffsetDateTime::now_utc();
    let mut initial_history_writer: Option<HistoryWriter> = None;
    let mut history_dir: Option<PathBuf> = None;
    if let Some(root) = config.history_root.as_deref() {
        let commit = config
            .current_commit
            .and_then(|f| f())
            .unwrap_or_else(|| "unknown".to_string());
        let metadata = crate::history::RunMetadata {
            run: crate::history::RunMetadataInner {
                started_at: now_for_history,
                commit,
                build_hash: None,
                requested_repeat: runs,
                fail_fast: config.fail_fast,
                scenarios: scenarios.iter().map(|s| s.name.to_string()).collect(),
                hostname: crate::history::current_hostname(),
                jobs: Some(config.jobs),
                cpu_jobs: Some(config.cpu_jobs),
                invocation: config.invocation.clone(),
            },
        };
        match crate::history::create_run_dir(root, &metadata) {
            Ok((dir, writer)) => {
                eprintln!("history: writing per-iteration records to {}", dir.display());
                history_dir = Some(dir);
                initial_history_writer = Some(writer);
            }
            Err(e) => {
                eprintln!(
                    "warning: failed to create history directory under {}: {e} — \
                     proceeding without per-iteration history.",
                    root.display()
                );
            }
        }
    }

    let history_writer: Mutex<Option<HistoryWriter>> = Mutex::new(initial_history_writer);
    let jobs = config.jobs.max(1);

    // Stress mode: single scenario × `--repeat N` iterations in
    // parallel. The plan's "deferred" extension. Kicks in when there
    // are N>1 iterations of exactly one scenario AND jobs > 1.
    // Fail-fast still works via a stop flag; SIGINT still works via
    // the external INTERRUPT atomic. All N iterations land in the
    // aggregator before the per-iteration loop below runs (which
    // is skipped because we set runs=0 by returning early).
    if scenarios.len() == 1 && runs > 1 && jobs > 1 {
        let s = scenarios[0];
        let workers = match s.cpu_profile {
            CpuProfile::Wfi => jobs,
            CpuProfile::Cpu => config.cpu_jobs.max(1),
        };
        eprintln!(
            "\n=== stress mode: {} × {} iterations of {} ===",
            workers, runs, s.name
        );
        let iter_wall_start = Instant::now();

        // Fail-fast: workers check this between pops. We set it when
        // cumulative failures cross the threshold. Distinct from
        // config.interrupt so SIGINT vs fail-fast produce different
        // exit codes.
        let stop = std::sync::atomic::AtomicBool::new(false);
        let failures_so_far = std::sync::atomic::AtomicU32::new(0);
        let fail_fast = config.fail_fast;
        let stop_ref = &stop;
        let failures_ref = &failures_so_far;

        // Fail-fast counts are kept coherent across workers via the
        // shared `failures_so_far` atomic each worker bumps after a
        // failing `process_one_scenario`. This stress path uses its own
        // worker pool (rather than `run_parallel_batch`) to keep the
        // single-scenario × N-iterations bookkeeping contained.
        let work_items: Vec<(u32, &Scenario)> =
            (0..runs).map(|i| (i, s)).collect();
        let work: Mutex<VecDeque<(u32, &Scenario)>> =
            Mutex::new(work_items.into_iter().collect());
        let work_ref = &work;
        let run_group = config.run_group;
        let history_dir_ref = history_dir.as_deref();
        let history_writer_ref = &history_writer;
        let interrupt = config.interrupt;

        let per_worker: Vec<Aggregator> = thread::scope(|sc| {
            let mut handles = Vec::with_capacity(workers as usize);
            for _ in 0..workers {
                handles.push(sc.spawn(move || {
                    let mut worker_agg = Aggregator::new();
                    loop {
                        if stop_ref.load(std::sync::atomic::Ordering::SeqCst) {
                            break;
                        }
                        if interrupt
                            .is_some_and(|f| f.load(std::sync::atomic::Ordering::SeqCst))
                        {
                            break;
                        }
                        let next = work_ref
                            .lock()
                            .expect("work-queue poisoned")
                            .pop_front();
                        let Some((iter_idx, s)) = next else { break };
                        let res = process_one_scenario(
                            s,
                            iter_idx,
                            &mut worker_agg,
                            history_writer_ref,
                            history_dir_ref,
                            run_group,
                            ScenarioFormat::Prefixed,
                        );
                        let totals = if res.failed {
                            let n = failures_ref
                                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                                + 1;
                            if let Some(k) = fail_fast
                                && n >= k
                            {
                                stop_ref
                                    .store(true, std::sync::atomic::Ordering::SeqCst);
                            }
                            RunTotals { passed: 0, failed: 1 }
                        } else {
                            RunTotals { passed: 1, failed: 0 }
                        };
                        worker_agg.finish_run(totals);
                    }
                    worker_agg
                }));
            }
            handles
                .into_iter()
                .map(|h| h.join().expect("worker panicked"))
                .collect()
        });

        for w in per_worker {
            aggregator.merge(w);
        }

        let stopped_early = stop.load(std::sync::atomic::Ordering::SeqCst);
        if stopped_early
            && let Some(k) = fail_fast
        {
            eprintln!(
                "\n--fail-fast: {} total failures reached (threshold {k}); stopped early.",
                aggregator.total_failures()
            );
        }
        if interrupt.is_some_and(|f| f.load(std::sync::atomic::Ordering::SeqCst)) {
            interrupted = true;
            eprintln!(
                "\nInterrupted during stress run. Writing partial baseline (if --update-baseline)."
            );
        }

        let iter_wall = iter_wall_start.elapsed();
        let iter_cpu = aggregator.total_duration();
        let completed = aggregator.runs();
        let total_failed = aggregator.total_failures();
        let passed_count = completed.saturating_sub(total_failed);
        eprintln!(
            "\nstress: {}/{} iterations passed, {} failed in {:.1} seconds wall time, {:.1} seconds CPU time",
            passed_count,
            completed,
            total_failed,
            iter_wall.as_secs_f64(),
            iter_cpu.as_secs_f64(),
        );

        // Skip the per-iteration loop below — stress mode owns the
        // aggregator state in full. Continue to baseline / pending
        // / exit-code logic via a labelled jump.
        return finalize_run(
            scenarios,
            runs,
            update_baseline,
            config,
            &aggregator,
            interrupted,
            history_dir.as_deref(),
        );
    }

    for run_idx in 0..runs {
        if runs > 1 {
            eprintln!("\n=== run {}/{} ===", run_idx + 1, runs);
        }

        // Step 3 of plans/legacy/itest-parallel-scenarios.md: fan-out is
        // intra-iteration. `jobs == 1` keeps the sequential output
        // format ("test X ... ok"); `jobs > 1` runs N worker threads
        // pulling from a shared queue, prefix-line output.
        let iter_wall_start = Instant::now();
        let mut local = Aggregator::new();
        let total = scenarios.len();

        // Group for execution: separate mode → singletons; shared mode →
        // by workload. The work unit downstream is the group.
        let groups = group_scenarios(scenarios, config.shared);

        let failed_this_run: usize = if jobs <= 1 {
            // Sequential path: one group at a time, each group's members
            // run against one subject.
            let mut count = 0;
            for group in &groups {
                let outcomes = process_group(
                    group,
                    run_idx,
                    &mut local,
                    &history_writer,
                    history_dir.as_deref(),
                    config.run_group,
                    ScenarioFormat::Inline,
                );
                count += outcomes.iter().filter(|o| o.failed).count();
            }
            count
        } else {
            // Parallel path. Partition GROUPS by `cpu_profile` (a group is
            // Cpu-bound if *any* member is): Wfi groups fan out across
            // `jobs` workers; Cpu groups run in a second pass at the
            // (typically smaller) `cpu_jobs` width so each lands on its own
            // host core.
            let (wfi_groups, cpu_groups): (Vec<Vec<&Scenario>>, Vec<Vec<&Scenario>>) =
                groups
                    .into_iter()
                    .partition(|g| g.iter().all(|s| matches!(s.cpu_profile, CpuProfile::Wfi)));

            // Cpu batch runs serially by default (`cpu_jobs` floored
            // to 1). Halving was removed: Cpu scenarios run real guest
            // work and must not contend with each other. Set
            // `cpu_jobs > 1` explicitly to parallelise the batch.
            let cpu_jobs = config.cpu_jobs.max(1);

            let mut iter_failed = 0usize;

            if !wfi_groups.is_empty() {
                let work: Vec<(u32, Vec<&Scenario>)> =
                    wfi_groups.into_iter().map(|g| (run_idx, g)).collect();
                let (wfi_agg, wfi_failed) = run_parallel_batch(
                    work,
                    jobs,
                    &history_writer,
                    history_dir.as_deref(),
                    config.run_group,
                    None,
                );
                local.merge(wfi_agg);
                iter_failed += wfi_failed;
            }

            if !cpu_groups.is_empty() {
                eprintln!(
                    "--- cpu-bound scenarios ({cpu_jobs} parallel) ---"
                );
                let work: Vec<(u32, Vec<&Scenario>)> =
                    cpu_groups.into_iter().map(|g| (run_idx, g)).collect();
                let (cpu_agg, cpu_failed) = run_parallel_batch(
                    work,
                    cpu_jobs,
                    &history_writer,
                    history_dir.as_deref(),
                    config.run_group,
                    None,
                );
                local.merge(cpu_agg);
                iter_failed += cpu_failed;
            }

            iter_failed
        };

        let iter_wall = iter_wall_start.elapsed();
        let iter_cpu = local.total_duration();
        eprintln!(
            "\n{} passed, {} failed in {:.1} seconds wall time, {:.1} seconds CPU time",
            total - failed_this_run,
            failed_this_run,
            iter_wall.as_secs_f64(),
            iter_cpu.as_secs_f64(),
        );
        local.finish_run(RunTotals {
            passed: total - failed_this_run,
            failed: failed_this_run,
        });
        aggregator.merge(local);

        // `--fail-fast=K`: abort the outer loop once K total failures
        // have accumulated. Print a one-liner so the user sees why
        // the run ended early.
        if let Some(k) = config.fail_fast
            && aggregator.total_failures() >= k
        {
            eprintln!(
                "\n--fail-fast: {} total failures reached (threshold {k}); aborting after run {}/{}.",
                aggregator.total_failures(),
                run_idx + 1,
                runs
            );
            break;
        }

        // Graceful interrupt: external signal handler flipped this
        // bool. Finish printing the per-run summary above, then exit
        // the loop. Pending-baseline write happens below.
        if let Some(flag) = config.interrupt
            && flag.load(std::sync::atomic::Ordering::SeqCst)
        {
            interrupted = true;
            eprintln!(
                "\nInterrupted after run {}/{}. Writing partial baseline (if --update-baseline).",
                run_idx + 1,
                runs
            );
            break;
        }
    }

    finalize_run(
        scenarios,
        runs,
        update_baseline,
        config,
        &aggregator,
        interrupted,
        history_dir.as_deref(),
    )
}

/// Post-loop logic shared by the per-iteration path and stress mode:
/// render the aggregate, compare against baseline, optionally write
/// a new baseline (or pending sidecar if interrupted), return the
/// exit code.
fn finalize_run(
    scenarios: &[&Scenario],
    runs: u32,
    update_baseline: bool,
    config: &RunnerConfig<'_>,
    aggregator: &Aggregator,
    interrupted: bool,
    history_dir: Option<&Path>,
) -> ExitCode {
    // Single-run path: no multi-run aggregate, but a one-off run that
    // flakes still reports which cause-bucket(s) it hit.
    if runs == 1 {
        return if aggregator.run_totals()[0].failed == 0 {
            ExitCode::SUCCESS
        } else {
            eprint!("{}", aggregator.render_signature_breakdown());
            ExitCode::from(1)
        };
    }

    eprint!("{}", aggregator.render_aggregate(runs));

    // Baseline comparison + optional update.
    let baseline_file = config
        .baseline_file
        .as_deref()
        .and_then(load_baseline_or_warn);
    print_baseline_comparisons(scenarios, aggregator, baseline_file.as_ref());

    if update_baseline {
        let commit = config
            .current_commit
            .and_then(|f| f())
            .unwrap_or_else(|| "unknown".to_string());
        let now = OffsetDateTime::now_utc();

        if interrupted {
            let Some(path) = config.pending_baseline.as_deref() else {
                eprintln!(
                    "warning: interrupted with --update-baseline but no \
                     pending_baseline path was supplied"
                );
                return exit_code_with_interrupt(aggregator, interrupted);
            };
            let partial = crate::baseline::PartialMarker {
                requested_runs: runs,
                interrupted_at: now,
                run_dir: history_dir.map(|p| p.to_string_lossy().into_owned()),
            };
            let pending = apply_current_run_to_baseline_with_partial(
                BaselineFile::default(),
                scenarios,
                aggregator,
                &commit,
                now,
                Some(&partial),
            );
            if let Err(e) = pending.save_path(path) {
                eprintln!("warning: failed to write {}: {e}", path.display());
            } else {
                eprintln!(
                    "\nWrote partial baseline to {} ({} of {} requested iterations). \
                     Promote with `baseline promote` or discard with `baseline discard`.",
                    path.display(),
                    aggregator.runs(),
                    runs
                );
            }
        } else {
            let Some(path) = config.baseline_file.as_deref() else {
                eprintln!(
                    "warning: --update-baseline requested but no baseline_file path was supplied"
                );
                return exit_code_with_interrupt(aggregator, interrupted);
            };
            let updated = apply_current_run_to_baseline_with_partial(
                baseline_file.unwrap_or_default(),
                scenarios,
                aggregator,
                &commit,
                now,
                None,
            );
            if let Err(e) = updated.save_path(path) {
                eprintln!("warning: failed to write {}: {e}", path.display());
            } else {
                eprintln!("\nUpdated {} with current run's results.", path.display());
            }
        }
    }

    exit_code_with_interrupt(aggregator, interrupted)
}

/// Run a batch of (iteration, scenario) work items across `workers`
/// threads pulling from a shared queue. Used three ways:
///
/// - **Wfi-batch fan-out** within one iteration: all pairs share the
///   same `run_idx`; one item per scenario.
/// - **Cpu-batch fan-out** within one iteration: same shape as
///   above but at the typically-smaller `cpu_jobs` width.
/// - **Stress mode** (single scenario × `--repeat N`): pairs are
///   `(0, s), (1, s), ..., (N-1, s)`; one item per iteration.
///
/// Returns the merged per-batch `Aggregator` plus total failures.
/// The aggregator's `run_totals` is NOT populated here — callers
/// decide whether each completed item is its own "iteration" (stress
/// mode) or just one of many in a single iteration (Wfi/Cpu batch).
fn run_parallel_batch<'a>(
    work_items: Vec<(u32, Vec<&'a Scenario>)>,
    workers: u32,
    history_writer: &'a Mutex<Option<HistoryWriter>>,
    history_dir: Option<&'a Path>,
    run_group: RunGroupFn<'a>,
    stop_signal: Option<&'a std::sync::atomic::AtomicBool>,
) -> (Aggregator, usize) {
    if work_items.is_empty() || workers == 0 {
        return (Aggregator::new(), 0);
    }
    let work: Mutex<VecDeque<(u32, Vec<&'a Scenario>)>> =
        Mutex::new(work_items.into_iter().collect());
    let work_ref = &work;

    let per_worker: Vec<(Aggregator, usize)> = thread::scope(|sc| {
        let mut handles = Vec::with_capacity(workers as usize);
        for _ in 0..workers {
            handles.push(sc.spawn(move || {
                let mut worker_agg = Aggregator::new();
                let mut worker_failed = 0usize;
                loop {
                    if stop_signal
                        .is_some_and(|f| f.load(std::sync::atomic::Ordering::SeqCst))
                    {
                        break;
                    }
                    let next = work_ref
                        .lock()
                        .expect("work-queue poisoned")
                        .pop_front();
                    let Some((iter_idx, group)) = next else { break };
                    let outcomes = process_group(
                        &group,
                        iter_idx,
                        &mut worker_agg,
                        history_writer,
                        history_dir,
                        run_group,
                        ScenarioFormat::Prefixed,
                    );
                    worker_failed += outcomes.iter().filter(|o| o.failed).count();
                }
                (worker_agg, worker_failed)
            }));
        }
        handles
            .into_iter()
            .map(|h| h.join().expect("worker panicked"))
            .collect()
    });

    let total_failed = per_worker.iter().map(|(_, f)| *f).sum();
    let mut merged = Aggregator::new();
    for (worker_agg, _) in per_worker {
        merged.merge(worker_agg);
    }
    (merged, total_failed)
}

/// Output format for `process_one_scenario`. Sequential runs use
/// `Inline` (`test NAME ... ok`); parallel runs use `Prefixed`
/// (`[NAME] ok`) so concurrent completions don't interleave
/// half-printed lines.
#[derive(Clone, Copy)]
enum ScenarioFormat {
    Inline,
    Prefixed,
}

struct ScenarioOutcome {
    failed: bool,
}

/// Partition scenarios into execution groups. Separate mode (`shared ==
/// false`) makes each scenario its own singleton group — today's
/// one-subject-per-scenario semantics. Shared mode groups by `workload`
/// (first-seen order preserved for stable output), so same-workload
/// scenarios run against one subject. The executor handles either; only
/// the grouping differs.
fn group_scenarios<'a>(scenarios: &[&'a Scenario], shared: bool) -> Vec<Vec<&'a Scenario>> {
    if !shared {
        return scenarios.iter().map(|&s| vec![s]).collect();
    }
    let mut groups: Vec<Vec<&'a Scenario>> = Vec::new();
    for &s in scenarios {
        match groups.iter_mut().find(|g| g[0].workload == s.workload) {
            Some(g) => g.push(s),
            None => groups.push(vec![s]),
        }
    }
    groups
}

/// Run a whole group through the consumer's `run_group` executor (one
/// subject for the group), returning one report per member in order. When
/// no executor is supplied, fall back to `(s.run)()` per member with an
/// otherwise-empty report — the crate's own test path. The result is
/// normalised to exactly `group.len()` reports: a short executor result
/// pads with an error report so no scenario is silently dropped.
fn execute_group(group: &[&Scenario], run_group: RunGroupFn<'_>) -> Vec<ScenarioReport> {
    let mut produced = match run_group {
        Some(exec) => exec(group).into_iter(),
        None => group
            .iter()
            .map(|s| ScenarioReport {
                result: (s.run)(),
                max_wait: None,
                capture: None,
                log_path: None,
            })
            .collect::<Vec<_>>()
            .into_iter(),
    };
    group
        .iter()
        .map(|s| {
            produced.next().unwrap_or_else(|| ScenarioReport {
                result: Err(format!("executor returned no report for scenario '{}'", s.name)),
                max_wait: None,
                capture: None,
                log_path: None,
            })
        })
        .collect()
}

/// Execute a group (one subject) and record each member's result: timing,
/// log-tail dump on failure, log-file copy into the run-dir, NDJSON row.
/// The group's wall time is attributed evenly across its members (a
/// singleton — separate mode — gets the whole time, identical to before).
/// Shared between the sequential and parallel paths.
fn process_group(
    group: &[&Scenario],
    run_idx: u32,
    local: &mut Aggregator,
    history_writer: &Mutex<Option<HistoryWriter>>,
    history_dir: Option<&Path>,
    run_group: RunGroupFn<'_>,
    format: ScenarioFormat,
) -> Vec<ScenarioOutcome> {
    let started_at = OffsetDateTime::now_utc();
    let start = Instant::now();
    let reports = execute_group(group, run_group);
    let group_elapsed = start.elapsed();
    // Even split: a singleton gets the full time (separate mode, unchanged);
    // a shared group amortises the boot across cheap per-View scans.
    let per = group_elapsed
        .checked_div(u32::try_from(group.len()).unwrap_or(1).max(1))
        .unwrap_or(group_elapsed);
    group
        .iter()
        .zip(reports)
        .map(|(s, report)| {
            process_report(s, report, per, started_at, run_idx, local, history_writer, history_dir, format)
        })
        .collect()
}

/// Singleton convenience over `process_group` — used by the stress path,
/// which repeats one scenario rather than iterating a list.
fn process_one_scenario(
    s: &Scenario,
    run_idx: u32,
    local: &mut Aggregator,
    history_writer: &Mutex<Option<HistoryWriter>>,
    history_dir: Option<&Path>,
    run_group: RunGroupFn<'_>,
    format: ScenarioFormat,
) -> ScenarioOutcome {
    process_group(&[s], run_idx, local, history_writer, history_dir, run_group, format)
        .pop()
        .expect("process_group returns one outcome per member")
}

/// Record one scenario's outcome from its report: print the result line,
/// dump/copy the log on failure, classify the failure signature, append
/// the NDJSON row. `duration` is this scenario's attributed share of its
/// group's wall time. All per-scenario facts come from the report — no
/// thread-local scraping.
#[allow(clippy::too_many_arguments, reason = "runner plumbing; grouping these would just move the noise")]
fn process_report(
    s: &Scenario,
    report: ScenarioReport,
    duration: Duration,
    started_at: OffsetDateTime,
    run_idx: u32,
    local: &mut Aggregator,
    history_writer: &Mutex<Option<HistoryWriter>>,
    history_dir: Option<&Path>,
    format: ScenarioFormat,
) -> ScenarioOutcome {
    local.record_duration(s.name, duration);

    let ScenarioReport { result: outcome, max_wait, capture, log_path } = report;

    let timing_str = max_wait
        .map(|(actual, budget)| {
            format!(
                " (max wait {:.1}s of {:.0}s budget)",
                actual.as_secs_f64(),
                budget.as_secs_f64()
            )
        })
        .unwrap_or_default();

    // The status line prints atomically (one `eprintln`) so parallel
    // completions don't interleave half-lines. `status_prefix` carries the
    // scenario name; `prefix` is the indent for any sub-lines (the error).
    let status_prefix = match format {
        ScenarioFormat::Inline => format!("test {} ... ", s.name),
        ScenarioFormat::Prefixed => format!("[{}] ", s.name),
    };
    let prefix = match format {
        ScenarioFormat::Inline => String::new(),
        ScenarioFormat::Prefixed => format!("[{}] ", s.name),
    };

    let mut row_log: Option<String> = None;
    let mut row_signature: Option<crate::signature::Signature> = None;
    let mut failed = false;
    let (row_result, row_error) = match &outcome {
        Ok(()) => {
            eprintln!("{status_prefix}ok{timing_str}");
            (crate::history::ResultKind::Pass, None)
        }
        Err(e) => {
            eprintln!("{status_prefix}FAILED{timing_str}");
            eprintln!("{prefix}  {e}");
            let mut log_tail: Option<String> = None;
            if let Some(log_path) = &log_path {
                dump_log_tail(log_path);
                log_tail = std::fs::read_to_string(log_path).ok();
                if let Some(hdir) = history_dir {
                    let dest_name = format!("fail-{}-{}.log", s.name, run_idx + 1);
                    let dest = hdir.join(&dest_name);
                    match std::fs::copy(log_path, &dest) {
                        Ok(_) => row_log = Some(dest_name),
                        Err(e) => eprintln!(
                            "warning: failed to copy log to {}: {e}",
                            dest.display()
                        ),
                    }
                }
            }
            // The report's structured capture attributes the failure to a
            // cause-bucket. Always produces a signature on failure —
            // `Unknown` when evidence is thin — so no failure goes
            // unattributed.
            row_signature = Some(crate::signature::classify_failure(
                capture.as_ref(),
                Some(e),
                log_tail.as_deref(),
            ));
            // Persist the structured capture alongside the UART log.
            if let (Some(cap), Some(hdir)) = (&capture, history_dir)
                && let Err(e) =
                    crate::history::write_capture_sidecar(hdir, s.name, run_idx + 1, cap)
            {
                eprintln!("warning: failed to write capture sidecar: {e}");
            }
            failed = true;
            local.record_fail_with_signature(s.name, row_signature);
            (crate::history::ResultKind::Fail, Some(e.clone()))
        }
    };

    // Brief lock for the NDJSON append. Contention is negligible at
    // realistic worker counts (~10ms scenario, microseconds of lock).
    let row = crate::history::IterationRow {
        iteration: run_idx + 1,
        scenario: s.name.to_string(),
        started_at,
        duration_ms: duration.as_millis().min(u128::from(u32::MAX)) as u32,
        result: row_result,
        error: row_error,
        log: row_log,
        signature: row_signature,
    };
    if let Some(writer) = history_writer
        .lock()
        .expect("history writer mutex poisoned")
        .as_mut()
        && let Err(e) = writer.append(&row)
    {
        eprintln!("warning: failed to append history row: {e}");
    }

    ScenarioOutcome { failed }
}

fn exit_code(aggregator: &Aggregator) -> ExitCode {
    if aggregator.any_failures() {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

/// Conventional Unix exit code 130 = "terminated by SIGINT", regardless
/// of whether scenarios passed or failed within the truncated run.
fn exit_code_with_interrupt(aggregator: &Aggregator, interrupted: bool) -> ExitCode {
    if interrupted {
        ExitCode::from(130)
    } else {
        exit_code(aggregator)
    }
}

fn dump_log_tail(log_path: &std::path::Path) {
    let contents = match std::fs::read_to_string(log_path) {
        Ok(s) if !s.trim().is_empty() => s,
        Ok(_) => return,
        Err(e) => {
            eprintln!("  (failed to read log {}: {e})", log_path.display());
            return;
        }
    };
    eprintln!("  --- QEMU log ({}) ---", log_path.display());
    let lines: Vec<&str> = contents.lines().collect();
    let tail_start = lines.len().saturating_sub(80);
    for line in &lines[tail_start..] {
        eprintln!("  | {line}");
    }
    eprintln!("  --- end QEMU log ---");
}

fn load_baseline_or_warn(path: &std::path::Path) -> Option<BaselineFile> {
    if !path.exists() {
        return None;
    }
    match BaselineFile::load_path(path) {
        Ok(f) => Some(f),
        Err(e) => {
            eprintln!(
                "warning: failed to parse {}: {e} — proceeding without baseline.",
                path.display()
            );
            None
        }
    }
}

fn print_baseline_comparisons(
    scenarios: &[&Scenario],
    aggregator: &Aggregator,
    baseline_file: Option<&BaselineFile>,
) {
    let Some(file) = baseline_file else { return };
    eprintln!("\n=== baseline comparison ===");
    let runs = aggregator.runs();
    for s in scenarios {
        let failures = aggregator.fail_count(s.name);
        let baseline = file.current_for(s.name);
        let v = verdict(failures, runs, baseline, DEFAULT_ALPHA);
        eprint!(
            "{}",
            render_comparison(&ComparisonRender {
                scenario: s.name,
                failures,
                runs,
                mean_duration: aggregator.mean_duration(s.name),
                p95_duration: aggregator.p95_duration(s.name),
                baseline,
                verdict: v,
            })
        );
    }
}

fn apply_current_run_to_baseline_with_partial(
    mut file: BaselineFile,
    scenarios: &[&Scenario],
    aggregator: &Aggregator,
    commit: &str,
    now: OffsetDateTime,
    partial: Option<&crate::baseline::PartialMarker>,
) -> BaselineFile {
    let runs = aggregator.runs();
    for s in scenarios {
        let baseline = Baseline {
            commit: commit.to_string(),
            build_hash: None,
            runs,
            failures: aggregator.fail_count(s.name),
            recorded_at: now,
            mean_duration_ms: aggregator
                .mean_duration(s.name)
                .map(|d| d.as_secs_f64() * 1000.0),
            p95_duration_ms: aggregator
                .p95_duration(s.name)
                .map(|d| d.as_secs_f64() * 1000.0),
            partial: partial.cloned(),
            signature_counts: aggregator.signature_counts_for(s.name),
        };
        file.update_current(s.name, baseline);
    }
    file
}

#[cfg(test)]
mod tests {
    // The `*_pass`/`*_fail` helpers below are used as `Scenario` fn
    // pointers, whose type is fixed at `fn() -> Result<(), String>`, so
    // their always-`Ok`/`Err` returns are required, not redundant.
    #![allow(
        clippy::unnecessary_wraps,
        reason = "return type fixed by the Scenario fn-pointer signature"
    )]
    use super::*;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU32, Ordering};

    // The scenario `fn()` signature can't capture closures, so the
    // test fakes have to use static counters. That makes them
    // mutually exclusive — gate every test body on this lock so the
    // counters reflect the test's own execution.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    static PASS_COUNTER: AtomicU32 = AtomicU32::new(0);
    static FAIL_COUNTER: AtomicU32 = AtomicU32::new(0);
    static BUILD_COUNTER: AtomicU32 = AtomicU32::new(0);

    fn always_pass() -> Result<(), String> {
        PASS_COUNTER.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    fn always_fail() -> Result<(), String> {
        FAIL_COUNTER.fetch_add(1, Ordering::Relaxed);
        Err("scripted failure".to_string())
    }

    fn reset_counters() {
        PASS_COUNTER.store(0, Ordering::Relaxed);
        FAIL_COUNTER.store(0, Ordering::Relaxed);
        BUILD_COUNTER.store(0, Ordering::Relaxed);
    }

    fn slow_pass() -> Result<(), String> {
        PASS_COUNTER.fetch_add(1, Ordering::Relaxed);
        std::thread::sleep(std::time::Duration::from_millis(200));
        Ok(())
    }

    #[test]
    fn parallel_jobs_run_scenarios_concurrently() {
        // Four slow scenarios sleeping 200ms each. Sequential = 800ms.
        // Parallel with jobs=4 should finish in roughly one sleep window
        // (~250ms), well under the sequential floor.
        let _guard = TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        reset_counters();
        let scns = [
            Scenario::new("slow-a", slow_pass),
            Scenario::new("slow-b", slow_pass),
            Scenario::new("slow-c", slow_pass),
            Scenario::new("slow-d", slow_pass),
        ];
        let refs: Vec<&Scenario> = scns.iter().collect();
        let config = RunnerConfig {
            jobs: 4,
            ..RunnerConfig::default()
        };
        let start = std::time::Instant::now();
        let _ = run(&refs, 1, false, &config);
        let elapsed = start.elapsed();

        assert_eq!(PASS_COUNTER.load(Ordering::Relaxed), 4);
        // Sequential would be ~800ms. Parallel-4 on the same scenarios
        // should be well under 500ms even on a busy CI runner. Slack
        // accounts for thread spawn + per-scenario bookkeeping.
        assert!(
            elapsed.as_millis() < 500,
            "expected parallel speedup; elapsed={elapsed:?}"
        );
    }

    fn slow_cpu_pass() -> Result<(), String> {
        PASS_COUNTER.fetch_add(1, Ordering::Relaxed);
        std::thread::sleep(std::time::Duration::from_millis(150));
        Ok(())
    }

    /// Track invocation timestamps so we can prove the Cpu serial pass
    /// fires after the Wfi parallel batch finishes.
    static CPU_INVOCATION_TIMES: Mutex<Vec<std::time::Instant>> = Mutex::new(Vec::new());
    static WFI_INVOCATION_TIMES: Mutex<Vec<std::time::Instant>> = Mutex::new(Vec::new());

    fn slow_cpu_tracked() -> Result<(), String> {
        PASS_COUNTER.fetch_add(1, Ordering::Relaxed);
        CPU_INVOCATION_TIMES.lock().unwrap().push(std::time::Instant::now());
        std::thread::sleep(std::time::Duration::from_millis(50));
        Ok(())
    }

    fn slow_wfi_tracked() -> Result<(), String> {
        PASS_COUNTER.fetch_add(1, Ordering::Relaxed);
        WFI_INVOCATION_TIMES.lock().unwrap().push(std::time::Instant::now());
        std::thread::sleep(std::time::Duration::from_millis(50));
        Ok(())
    }

    #[test]
    fn cpu_bound_batch_runs_after_wfi_batch() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        reset_counters();
        CPU_INVOCATION_TIMES.lock().unwrap().clear();
        WFI_INVOCATION_TIMES.lock().unwrap().clear();

        let scns = [
            Scenario::new("wfi-a", slow_wfi_tracked),
            Scenario::new("wfi-b", slow_wfi_tracked),
            Scenario::cpu_bound("cpu-a", slow_cpu_tracked),
            Scenario::cpu_bound("cpu-b", slow_cpu_tracked),
        ];
        let refs: Vec<&Scenario> = scns.iter().collect();
        // cpu_jobs=1 to keep the Cpu batch sequential for this test,
        // so the Wfi-then-Cpu ordering is what's actually being asserted.
        let config = RunnerConfig {
            jobs: 4,
            cpu_jobs: 1,
            ..RunnerConfig::default()
        };
        let _ = run(&refs, 1, false, &config);

        assert_eq!(PASS_COUNTER.load(Ordering::Relaxed), 4);
        let wfi_times = WFI_INVOCATION_TIMES.lock().unwrap().clone();
        let cpu_times = CPU_INVOCATION_TIMES.lock().unwrap().clone();
        assert_eq!(wfi_times.len(), 2);
        assert_eq!(cpu_times.len(), 2);

        // Every Cpu invocation should start AFTER both Wfi invocations
        // started (the Wfi batch finishes before the Cpu pass begins).
        let latest_wfi_start = *wfi_times.iter().max().unwrap();
        for cpu_start in &cpu_times {
            assert!(
                *cpu_start >= latest_wfi_start,
                "Cpu scenario started before Wfi batch finished"
            );
        }
    }

    #[test]
    fn cpu_jobs_zero_resolves_to_serial_not_halved() {
        // cpu_jobs=0 must run the Cpu batch *serially* (floored to 1),
        // never the old "half of jobs" auto-resolution. So two Cpu
        // scenarios take two sleep windows, not one — regardless of
        // `jobs`. (The CLI rejects 0; this guards the library floor
        // for default-constructed configs.)
        let _guard = TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        reset_counters();
        let scns = [
            Scenario::cpu_bound("cpu-a", slow_cpu_pass),
            Scenario::cpu_bound("cpu-b", slow_cpu_pass),
        ];
        let refs: Vec<&Scenario> = scns.iter().collect();
        let config = RunnerConfig {
            jobs: 4,
            cpu_jobs: 0, // must NOT auto-resolve to jobs/2
            ..RunnerConfig::default()
        };
        let start = std::time::Instant::now();
        let _ = run(&refs, 1, false, &config);
        let elapsed = start.elapsed();
        assert_eq!(PASS_COUNTER.load(Ordering::Relaxed), 2);
        // Serial: 2 × 150ms = 300ms minimum. Halving (the removed
        // behavior) would have run both at once in ~200ms.
        assert!(
            elapsed.as_millis() >= 280,
            "Cpu batch ran in parallel under cpu_jobs=0 (halving not removed?); elapsed={elapsed:?}"
        );
    }

    #[test]
    fn stress_mode_runs_single_scenario_iterations_in_parallel() {
        // Single scenario × 8 iterations at jobs=4 → 4 workers run
        // the iterations concurrently; wall-clock is roughly
        // ceil(8/4) * 200ms = ~400ms, well under the sequential
        // floor of 1600ms.
        let _guard = TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        reset_counters();
        let scns = [Scenario::new("stress-target", slow_pass)];
        let refs: Vec<&Scenario> = scns.iter().collect();
        let config = RunnerConfig {
            jobs: 4,
            ..RunnerConfig::default()
        };
        let start = std::time::Instant::now();
        let _ = run(&refs, 8, false, &config);
        let elapsed = start.elapsed();
        assert_eq!(PASS_COUNTER.load(Ordering::Relaxed), 8);
        // 8 × 200ms sequential = 1600ms. 4-wide parallel ~400ms.
        // Generous slack for spawn/merge overhead.
        assert!(
            elapsed.as_millis() < 900,
            "expected parallel stress; elapsed={elapsed:?}"
        );
    }

    #[test]
    fn stress_mode_inactive_when_repeat_is_one() {
        // repeat=1 should not enter stress mode — falls through to
        // the standard per-iteration loop with a single scenario.
        let _guard = TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        reset_counters();
        let scns = [Scenario::new("once", always_pass)];
        let refs: Vec<&Scenario> = scns.iter().collect();
        let config = RunnerConfig {
            jobs: 8,
            ..RunnerConfig::default()
        };
        let _ = run(&refs, 1, false, &config);
        assert_eq!(PASS_COUNTER.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn stress_mode_inactive_when_jobs_is_one() {
        // jobs=1 means sequential — no stress fan-out even with
        // single scenario × repeat>1.
        let _guard = TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        reset_counters();
        let scns = [Scenario::new("once", slow_pass)];
        let refs: Vec<&Scenario> = scns.iter().collect();
        let config = RunnerConfig {
            jobs: 1,
            ..RunnerConfig::default()
        };
        let start = std::time::Instant::now();
        let _ = run(&refs, 4, false, &config);
        let elapsed = start.elapsed();
        assert_eq!(PASS_COUNTER.load(Ordering::Relaxed), 4);
        // Should be ~800ms sequential, not ~200ms parallel.
        assert!(
            elapsed.as_millis() >= 700,
            "expected sequential at jobs=1; elapsed={elapsed:?}"
        );
    }

    #[test]
    fn stress_mode_fail_fast_stops_workers_after_threshold() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        reset_counters();
        let scns = [Scenario::new("fail-1", always_fail)];
        let refs: Vec<&Scenario> = scns.iter().collect();
        let config = RunnerConfig {
            jobs: 4,
            fail_fast: Some(2),
            ..RunnerConfig::default()
        };
        let _ = run(&refs, 100, false, &config);
        // Workers may overshoot slightly (each takes ~one extra item
        // before observing the stop flag). Bound loosely: at least 2
        // (threshold) and at most threshold + jobs (one in-flight per
        // worker at the moment fail-fast trips).
        let n = FAIL_COUNTER.load(Ordering::Relaxed);
        assert!(
            (2..=2 + 4 + 1).contains(&n),
            "expected fail-fast around threshold; got {n}"
        );
    }

    #[test]
    fn cpu_jobs_one_keeps_cpu_batch_serial() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        reset_counters();
        let scns = [
            Scenario::cpu_bound("cpu-a", slow_cpu_pass),
            Scenario::cpu_bound("cpu-b", slow_cpu_pass),
            Scenario::cpu_bound("cpu-c", slow_cpu_pass),
        ];
        let refs: Vec<&Scenario> = scns.iter().collect();
        let config = RunnerConfig {
            jobs: 4,
            cpu_jobs: 1,
            ..RunnerConfig::default()
        };
        let start = std::time::Instant::now();
        let _ = run(&refs, 1, false, &config);
        let elapsed = start.elapsed();
        assert_eq!(PASS_COUNTER.load(Ordering::Relaxed), 3);
        // Serial expectation: 3 × 150ms = 450ms minimum.
        assert!(
            elapsed.as_millis() >= 400,
            "Cpu scenarios appear to have run in parallel under cpu_jobs=1; elapsed={elapsed:?}"
        );
    }

    #[test]
    fn parallel_jobs_aggregator_merge_matches_sequential() {
        // Same workload, jobs=1 vs jobs=4: total fail counts must match.
        let _guard = TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        reset_counters();
        let scns = [
            Scenario::new("pass-1", always_pass),
            Scenario::new("fail-1", always_fail),
            Scenario::new("pass-2", always_pass),
            Scenario::new("fail-2", always_fail),
        ];
        let refs: Vec<&Scenario> = scns.iter().collect();
        let parallel = RunnerConfig {
            jobs: 4,
            ..RunnerConfig::default()
        };
        let _ = run(&refs, 3, false, &parallel);
        // 3 iterations × 2 always_fail scenarios = 6 fails total.
        // 3 iterations × 2 always_pass scenarios = 6 passes total.
        assert_eq!(PASS_COUNTER.load(Ordering::Relaxed), 6);
        assert_eq!(FAIL_COUNTER.load(Ordering::Relaxed), 6);
    }

    #[test]
    fn run_with_only_passing_scenarios_invokes_scenarios_n_times() {
        // Don't `unwrap` — a panicking test poisons the mutex; we don't
        // want one failure to cascade through every other runner test.
        let _guard = TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        reset_counters();
        let s = Scenario::new("pass-1", always_pass);
        let _ = run(&[&s], 3, false, &RunnerConfig::default());
        assert_eq!(PASS_COUNTER.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn run_with_one_failing_scenario_invokes_both_each_iteration() {
        // Don't `unwrap` — a panicking test poisons the mutex; we don't
        // want one failure to cascade through every other runner test.
        let _guard = TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        reset_counters();
        let pass = Scenario::new("pass-1", always_pass);
        let fail = Scenario::new("fail-1", always_fail);
        let _ = run(&[&pass, &fail], 2, false, &RunnerConfig::default());
        assert_eq!(PASS_COUNTER.load(Ordering::Relaxed), 2);
        assert_eq!(FAIL_COUNTER.load(Ordering::Relaxed), 2);
    }

    fn failing_build() -> Result<(), String> {
        BUILD_COUNTER.fetch_add(1, Ordering::Relaxed);
        Err("kernel build broke".to_string())
    }

    #[test]
    fn build_hook_failure_aborts_run() {
        // Don't `unwrap` — a panicking test poisons the mutex; we don't
        // want one failure to cascade through every other runner test.
        let _guard = TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        reset_counters();
        let s = Scenario::new("pass-1", always_pass);
        let config = RunnerConfig {
            one_shot_build: Some(&failing_build),
            ..RunnerConfig::default()
        };
        let _ = run(&[&s], 5, false, &config);
        assert_eq!(BUILD_COUNTER.load(Ordering::Relaxed), 1);
        // Scenario should never have been called.
        assert_eq!(PASS_COUNTER.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn failure_log_copied_into_run_dir_and_referenced_in_ndjson() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        reset_counters();
        let fail = Scenario::new("fail-1", always_fail);

        // Synthesise a log file that the executor's report will point at.
        let scratch = std::env::temp_dir().join(format!(
            "itest-harness-fail-log-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&scratch);
        std::fs::create_dir_all(&scratch).unwrap();
        let log_path = scratch.join("fake-qemu.log");
        std::fs::write(&log_path, "synthetic log contents\n").unwrap();
        // Executor: run the scenario and report the synthetic log path —
        // the runner copies it into the run-dir on failure.
        let run_group = |scns: &[&Scenario]| -> Vec<ScenarioReport> {
            scns.iter()
                .map(|s| ScenarioReport {
                    result: (s.run)(),
                    max_wait: None,
                    capture: None,
                    log_path: Some(log_path.clone()),
                })
                .collect()
        };

        let history_root = scratch.join("history");
        std::fs::create_dir_all(&history_root).unwrap();
        let config = RunnerConfig {
            run_group: Some(&run_group),
            history_root: Some(history_root.clone()),
            ..RunnerConfig::default()
        };
        let _ = run(&[&fail], 1, false, &config);

        // Find the run dir.
        let entries: Vec<_> = std::fs::read_dir(&history_root)
            .unwrap()
            .filter_map(std::result::Result::ok)
            .collect();
        assert_eq!(entries.len(), 1);
        let run_dir = entries[0].path();

        // Log file copied with the expected name.
        let copied = run_dir.join("fail-fail-1-1.log");
        assert!(copied.exists(), "log file should be copied to run dir");
        let copied_content = std::fs::read_to_string(&copied).unwrap();
        assert_eq!(copied_content, "synthetic log contents\n");

        // NDJSON row references it.
        let rows: Vec<_> = crate::history::read_iterations(&run_dir.join("iterations.ndjson"))
            .unwrap()
            .collect::<std::io::Result<_>>()
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].log.as_deref(), Some("fail-fail-1-1.log"));

        let _ = std::fs::remove_dir_all(&scratch);
    }

    #[test]
    fn history_root_writes_metadata_and_ndjson() {
        // Don't `unwrap` — a panicking test poisons the mutex; we don't
        // want one failure to cascade through every other runner test.
        let _guard = TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        reset_counters();
        let pass = Scenario::new("pass-1", always_pass);
        let fail = Scenario::new("fail-1", always_fail);

        let root = std::env::temp_dir().join(format!(
            "itest-harness-runner-history-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        let config = RunnerConfig {
            history_root: Some(root.clone()),
            ..RunnerConfig::default()
        };
        let _ = run(&[&pass, &fail], 2, false, &config);

        // Find the single run subdir.
        let entries: Vec<_> = std::fs::read_dir(&root)
            .unwrap()
            .filter_map(std::result::Result::ok)
            .collect();
        assert_eq!(entries.len(), 1, "expected one run-dir under history root");
        let run_dir = entries[0].path();

        // metadata.toml exists with the right shape. We don't pin the
        // exact TOML formatting of `scenarios` (the toml crate may
        // wrap a vec of strings across lines); just check that both
        // names are present in the file.
        let meta = std::fs::read_to_string(run_dir.join("metadata.toml")).unwrap();
        assert!(meta.contains("requested_repeat = 2"));
        assert!(meta.contains("\"pass-1\""));
        assert!(meta.contains("\"fail-1\""));

        // iterations.ndjson has 4 rows (2 scenarios × 2 iterations).
        let ndjson_path = run_dir.join("iterations.ndjson");
        let rows: Vec<_> = crate::history::read_iterations(&ndjson_path)
            .unwrap()
            .collect::<std::io::Result<_>>()
            .unwrap();
        assert_eq!(rows.len(), 4);
        // First two rows: iteration 1, pass then fail.
        assert_eq!(rows[0].iteration, 1);
        assert_eq!(rows[0].scenario, "pass-1");
        assert_eq!(rows[0].result, crate::history::ResultKind::Pass);
        assert_eq!(rows[1].iteration, 1);
        assert_eq!(rows[1].scenario, "fail-1");
        assert_eq!(rows[1].result, crate::history::ResultKind::Fail);
        assert_eq!(rows[1].error.as_deref(), Some("scripted failure"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn interrupt_breaks_outer_loop_at_iteration_boundary() {
        // Don't `unwrap` — a panicking test poisons the mutex; we don't
        // want one failure to cascade through every other runner test.
        let _guard = TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        reset_counters();
        let pass = Scenario::new("pass-1", always_pass);
        // Pre-set the interrupt flag: the runner should observe it
        // after the first iteration finishes and break.
        let flag = std::sync::atomic::AtomicBool::new(true);
        let config = RunnerConfig {
            interrupt: Some(&flag),
            ..RunnerConfig::default()
        };
        let _ = run(&[&pass], 10, false, &config);
        // First iteration completes; interrupt check at boundary breaks.
        assert_eq!(PASS_COUNTER.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn interrupt_with_update_baseline_writes_pending_not_canonical() {
        // Don't `unwrap` — a panicking test poisons the mutex; we don't
        // want one failure to cascade through every other runner test.
        let _guard = TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        reset_counters();
        let pass = Scenario::new("pass-1", always_pass);

        let dir = std::env::temp_dir().join(format!(
            "itest-harness-pending-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let canonical = dir.join("baseline.toml");
        let pending = dir.join("baseline.toml.pending");
        // Pre-populate the canonical so we can later assert it was untouched.
        std::fs::write(&canonical, "# untouched\n").unwrap();

        let flag = std::sync::atomic::AtomicBool::new(true);
        let config = RunnerConfig {
            interrupt: Some(&flag),
            baseline_file: Some(canonical.clone()),
            pending_baseline: Some(pending.clone()),
            ..RunnerConfig::default()
        };
        let _ = run(&[&pass], 10, true, &config);

        // Canonical untouched.
        let canonical_after = std::fs::read_to_string(&canonical).unwrap();
        assert_eq!(canonical_after, "# untouched\n");
        // Pending written with the partial marker.
        let pending_after = std::fs::read_to_string(&pending).unwrap();
        assert!(pending_after.contains("[scenarios.pass-1.current.partial]"));
        assert!(pending_after.contains("requested_runs = 10"));
        // Only 1 iteration actually completed before the boundary check.
        assert!(pending_after.contains("runs = 1"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fail_fast_breaks_outer_loop_at_threshold() {
        // Don't `unwrap` — a panicking test poisons the mutex; we don't
        // want one failure to cascade through every other runner test.
        let _guard = TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        reset_counters();
        let fail = Scenario::new("fail-1", always_fail);
        // Without fail-fast, repeat=10 would invoke fail-1 ten times.
        // With fail-fast=3, the runner stops after the 3rd iteration.
        let config = RunnerConfig {
            fail_fast: Some(3),
            ..RunnerConfig::default()
        };
        let _ = run(&[&fail], 10, false, &config);
        assert_eq!(FAIL_COUNTER.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn fail_fast_does_not_trigger_when_threshold_not_reached() {
        // Don't `unwrap` — a panicking test poisons the mutex; we don't
        // want one failure to cascade through every other runner test.
        let _guard = TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        reset_counters();
        let fail = Scenario::new("fail-1", always_fail);
        // 5 failures requested, threshold of 100 — full run completes.
        let config = RunnerConfig {
            fail_fast: Some(100),
            ..RunnerConfig::default()
        };
        let _ = run(&[&fail], 5, false, &config);
        assert_eq!(FAIL_COUNTER.load(Ordering::Relaxed), 5);
    }

    #[test]
    fn fail_fast_none_runs_to_completion_with_failures() {
        // Don't `unwrap` — a panicking test poisons the mutex; we don't
        // want one failure to cascade through every other runner test.
        let _guard = TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        reset_counters();
        let fail = Scenario::new("fail-1", always_fail);
        let _ = run(&[&fail], 7, false, &RunnerConfig::default());
        assert_eq!(FAIL_COUNTER.load(Ordering::Relaxed), 7);
    }

    fn tagged_catalog() -> [Scenario; 4] {
        [
            Scenario::new("userspace-hello", always_pass).tagged(&["userspace"]),
            Scenario::new("smp-boot", always_pass).tagged(&["smp"]),
            Scenario::cpu_bound("spawn-storm", always_pass).tagged(&["smp", "stress"]),
            Scenario::new("boot-heartbeat", always_pass),
        ]
    }

    fn names(scns: &[&Scenario]) -> Vec<&'static str> {
        scns.iter().map(|s| s.name).collect()
    }

    #[test]
    fn select_by_tags_unions_matching_scenarios() {
        let catalog = tagged_catalog();
        let refs: Vec<&Scenario> = catalog.iter().collect();
        let selected = select_by_tags(&refs, &["smp".to_string()]).unwrap();
        assert_eq!(names(&selected), vec!["smp-boot", "spawn-storm"]);
    }

    #[test]
    fn select_by_tags_takes_the_union_of_multiple_tags() {
        let catalog = tagged_catalog();
        let refs: Vec<&Scenario> = catalog.iter().collect();
        let selected =
            select_by_tags(&refs, &["userspace".to_string(), "stress".to_string()]).unwrap();
        assert_eq!(names(&selected), vec!["userspace-hello", "spawn-storm"]);
    }

    #[test]
    fn select_by_tags_errors_loudly_on_an_unknown_tag() {
        let catalog = tagged_catalog();
        let refs: Vec<&Scenario> = catalog.iter().collect();
        // A tag carried by no scenario is almost always a typo; matching
        // nothing silently would hide it.
        let err = select_by_tags(&refs, &["usrspace".to_string()]).unwrap_err();
        assert!(err.contains("usrspace"), "error should name the bad tag: {err}");
    }

    #[test]
    fn select_by_tags_rejects_a_mix_of_known_and_unknown() {
        let catalog = tagged_catalog();
        let refs: Vec<&Scenario> = catalog.iter().collect();
        let err =
            select_by_tags(&refs, &["smp".to_string(), "bogus".to_string()]).unwrap_err();
        assert!(err.contains("bogus"), "error should name the bad tag: {err}");
    }

    #[test]
    fn scenario_builders_compose_profile_tags_and_workload() {
        // The catalog macro lives consumer-side now; these are the
        // `Scenario` builders it composes.
        let s = Scenario::cpu_bound("spawn-storm", always_pass)
            .tagged(&["smp", "stress"])
            .on_workload("spawn-storm");
        assert_eq!(s.name, "spawn-storm");
        assert_eq!(s.cpu_profile, CpuProfile::Cpu);
        assert_eq!(s.tags, ["smp", "stress"].as_slice());
        assert_eq!(s.workload, Some("spawn-storm"));

        let plain = Scenario::new("boot", always_pass);
        assert_eq!(plain.cpu_profile, CpuProfile::Wfi);
        assert!(plain.tags.is_empty());
        assert_eq!(plain.workload, None);
    }

    #[test]
    fn group_scenarios_separate_makes_singletons() {
        let a = Scenario::new("a", always_pass).on_workload("x");
        let b = Scenario::new("b", always_pass).on_workload("x");
        let refs = [&a, &b];
        let groups = group_scenarios(&refs, false);
        assert_eq!(groups.len(), 2, "separate mode: one group per scenario");
        assert!(groups.iter().all(|g| g.len() == 1));
    }

    #[test]
    fn group_scenarios_shared_groups_by_workload_preserving_order() {
        let a = Scenario::new("a", always_pass).on_workload("x");
        let b = Scenario::new("b", always_pass); // default demo (None)
        let c = Scenario::new("c", always_pass).on_workload("x");
        let d = Scenario::new("d", always_pass); // None
        let refs = [&a, &b, &c, &d];
        let groups = group_scenarios(&refs, true);
        // First-seen workload order: the "x" group, then the None group.
        assert_eq!(groups.len(), 2);
        assert_eq!(names(&groups[0]), vec!["a", "c"]);
        assert_eq!(names(&groups[1]), vec!["b", "d"]);
    }

    #[test]
    fn shared_mode_hands_same_workload_scenarios_to_the_executor_as_one_group() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        reset_counters();
        let a = Scenario::new("a", always_pass).on_workload("w");
        let b = Scenario::new("b", always_pass).on_workload("w");
        let c = Scenario::new("c", always_pass); // default demo

        // The executor records the size of each group it's handed.
        let group_sizes = Mutex::new(Vec::new());
        let run_group = |scns: &[&Scenario]| -> Vec<ScenarioReport> {
            group_sizes.lock().unwrap().push(scns.len());
            scns.iter()
                .map(|s| ScenarioReport {
                    result: (s.run)(),
                    max_wait: None,
                    capture: None,
                    log_path: None,
                })
                .collect()
        };
        let config = RunnerConfig {
            shared: true,
            run_group: Some(&run_group),
            ..RunnerConfig::default()
        };
        let _ = run(&[&a, &b, &c], 1, false, &config);

        let mut sizes = group_sizes.lock().unwrap().clone();
        sizes.sort_unstable();
        assert_eq!(sizes, vec![1, 2], "{{a,b}} is one group of 2, {{c}} a group of 1");
        // Every scenario still ran exactly once.
        assert_eq!(PASS_COUNTER.load(Ordering::Relaxed), 3);
    }
}
