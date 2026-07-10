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
use super::{SCENARIOS, scenario_view_fn};
use crate::snemu_diff;

/// One scenario's audit row: its outcome plus the deterministic guest-instruction
/// cost of reaching it. `steps` is what the slowest-scenario table sorts by —
/// contention-free (unlike wall-clock), so it pinpoints where the CPU goes.
struct Row {
    name: &'static str,
    outcome: Outcome,
    steps: u64,
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
    idle_skip: bool,
    lpt: bool,
    release: bool,
) -> ExitCode {
    let (kernel, dtb) = match snemu_diff::prepare_profiled(true, release) {
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
         up to {max_steps} steps each, {jobs} worker(s), idle-skip {}",
        SCENARIOS.len(),
        if idle_skip { "on" } else { "off" },
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
    let booted = parallel_map(&distinct, jobs, |worker, _, &workload| {
        let start_s = started.elapsed().as_secs_f64();
        let boot_start = Instant::now();
        let snapshot = boot_snapshot(&kernel, &dtb, workload, idle_skip);
        worker_busy[worker].fetch_add(boot_start.elapsed().as_nanos() as u64, Ordering::Relaxed);
        let boot_steps = snapshot.as_ref().map_or(0, |(_, n)| *n);
        segments.lock().expect("segments mutex").push(Segment {
            kind: "boot",
            name: workload.unwrap_or("(default)").to_owned(),
            workload: workload.map(str::to_owned),
            worker,
            start_s,
            end_s: started.elapsed().as_secs_f64(),
            instret: boot_steps,
            pass: true,
        });
        (workload, snapshot.map(|(m, _)| m), boot_steps)
    });
    let boot_steps: u64 = booted.iter().map(|(_, _, n)| *n).sum();
    let snapshots: std::collections::HashMap<Option<&str>, Result<snemu::machine::Machine, String>> =
        booted.into_iter().map(|(w, snap, _)| (w, snap)).collect();

    // Phase 2 — scenario-level fan-out. This is what fixes the packing: because the
    // snapshot is shared, each *scenario* (not each workload group) is an
    // independent unit of work, so a workload's scenarios spread across all workers
    // instead of stacking on one. LPT order (heaviest first, by the previous run's
    // deterministic instret) keeps a slow scenario off the tail; unknowns sort
    // first. `--no-lpt` keeps selection order for the A/B baseline.
    let mut order: Vec<(usize, &itest_harness::Scenario)> = work.iter().copied().enumerate().collect();
    if lpt {
        let history = load_durations();
        let heaviest = history.values().copied().max().unwrap_or(0);
        order.sort_by_key(|(_, s)| {
            std::cmp::Reverse(history.get(s.name).copied().unwrap_or(heaviest))
        });
    }

    let done = AtomicUsize::new(0);
    let scenario_results = parallel_map(&order, jobs, |worker, _, &(index, s)| {
        // Fork the shared post-boot snapshot; fall back to a fresh boot for an
        // early-crash workload that never reached the checkpoint.
        let machine = match snapshots.get(&s.workload) {
            Some(Ok(snapshot)) => Some(snapshot.clone()),
            _ => match snemu_diff::load_workload_machine(&kernel, &dtb, s.workload) {
                Ok(mut m) => {
                    m.set_idle_skip(idle_skip);
                    Some(m)
                }
                Err(_) => None,
            },
        };
        let scenario_start = Instant::now();
        let start_s = started.elapsed().as_secs_f64();
        let (outcome, steps) = match machine {
            Some(m) => run_scenario(m, s, max_steps),
            None => (
                Outcome::Fail { why: "snemu load failed".to_owned(), console: String::new() },
                0,
            ),
        };
        worker_busy[worker].fetch_add(scenario_start.elapsed().as_nanos() as u64, Ordering::Relaxed);
        let wall = scenario_start.elapsed().as_secs_f64();
        let pass = matches!(outcome, Outcome::Pass);
        segments.lock().expect("segments mutex").push(Segment {
            kind: "scenario",
            name: s.name.to_owned(),
            workload: s.workload.map(str::to_owned),
            worker,
            start_s,
            end_s: started.elapsed().as_secs_f64(),
            instret: steps,
            pass,
        });
        let n = done.fetch_add(1, Ordering::SeqCst) + 1;
        eprintln!(
            "snemu-itest: [{n:>3}/{cap}] w{worker:<2} {:<40} {:<4} {:>6}M {wall:>6.2}s",
            s.name,
            if pass { "ok" } else { "FAIL" },
            steps / 1_000_000,
        );
        (index, Row { name: s.name, outcome, steps })
    });

    // Restore selection order via each scenario's original index.
    let mut rows: Vec<Option<Row>> = (0..work.len()).map(|_| None).collect();
    for (index, row) in scenario_results {
        rows[index] = Some(row);
    }
    let results: Vec<Row> = rows.into_iter().flatten().collect();
    let makespan = started.elapsed();

    // Persist this run's per-scenario instret as the packing predictor for next
    // time (a full run overwrites; a filtered `--only` run merges, preserving the
    // durations it didn't touch).
    save_durations(&results);

    // Emit the machine-readable packing snapshot for the `viz/` renderer.
    let scenario_instret: u64 = results.iter().map(|r| r.steps).sum();
    write_packing_report(
        lpt,
        jobs,
        makespan,
        scenario_instret + boot_steps,
        boot_steps,
        &worker_busy,
        segments.into_inner().expect("segments mutex"),
    );

    let exit = print_report(&results, boot_steps, makespan.as_secs_f64());
    print_utilization(&worker_busy, makespan, lpt);
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
    lpt: bool,
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
        packing: if lpt { "LPT" } else { "selection-order" },
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

/// Report each worker's utilization — busy time over the wall-clock makespan — so
/// the packing quality is visible. A well-packed run has every worker near the
/// bottleneck's 100%; a low minimum means one core was left running a straggler
/// while the others finished. Comparing this between `--no-lpt` and the default
/// (LPT) is the A/B for the packing change.
fn print_utilization(worker_busy: &[AtomicU64], makespan: Duration, lpt: bool) {
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
        if lpt { "LPT" } else { "selection-order" },
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
}

/// Where per-scenario instret is cached between runs, to drive LPT packing. Repo
/// root, gitignored; a plain `<instret> <name>` line per scenario.
const DURATIONS_PATH: &str = ".snemu-itest-durations";

/// Load the cached per-scenario instret (scenario name → guest instructions).
/// Missing or unparsable file → empty map (first run packs in selection order).
fn load_durations() -> HashMap<String, u64> {
    let Ok(text) = std::fs::read_to_string(DURATIONS_PATH) else {
        return HashMap::new();
    };
    text.lines()
        .filter_map(|line| {
            let (steps, name) = line.split_once(char::is_whitespace)?;
            Some((name.trim().to_owned(), steps.trim().parse().ok()?))
        })
        .collect()
}

/// Write this run's per-scenario instret back to the cache, merged over any prior
/// entries (so a `--only` subset run doesn't erase durations for scenarios it
/// skipped). Best-effort: a write failure just means next run packs from stale or
/// partial history, never a hard error.
fn save_durations(results: &[Row]) {
    let mut durations = load_durations();
    for r in results {
        // Record every scenario, including the 0-step ones whose assertion is
        // already satisfied from the forked checkpoint's frame buffer — they're
        // legitimately cheap, and omitting them makes LPT mistake them for
        // unknown-and-therefore-heaviest next run.
        durations.insert(r.name.to_owned(), r.steps);
    }
    let mut lines: Vec<(String, u64)> = durations.into_iter().collect();
    lines.sort_by_key(|(_, steps)| std::cmp::Reverse(*steps));
    let mut body = String::new();
    for (name, steps) in &lines {
        let _ = writeln!(body, "{steps} {name}");
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
    idle_skip: bool,
) -> Result<(snemu::machine::Machine, u64), String> {
    let mut machine = snemu_diff::load_workload_machine(kernel, dtb, workload)?;
    machine.set_idle_skip(idle_skip);
    let steps = machine.run_until_uart(CHECKPOINT, CHECKPOINT_BUDGET)?;
    Ok((machine, steps))
}

/// Drive `scenario` against `machine` (a fresh or forked live machine), returning
/// its outcome and the guest-instruction cost of reaching it.
fn run_scenario(
    machine: snemu::machine::Machine,
    scenario: &itest_harness::Scenario,
    max_steps: u64,
) -> (Outcome, u64) {
    let mut view = View::live(machine, budget_for(scenario.name, max_steps));
    let outcome = match scenario_view_fn(scenario.name)(&mut view) {
        Ok(()) => Outcome::Pass,
        Err(why) => Outcome::Fail {
            why,
            console: console_tail(view.console_output().unwrap_or_default().as_str()),
        },
    };
    (outcome, view.steps_taken().unwrap_or(0))
}

/// Per-scenario step-budget override. Most scenarios reach their assertion well
/// under the default; a handful are genuinely **budget-bound** under snemu's
/// instruction clock — their thresholds (N consumed samples, an OOM, a reaper
/// completing) require the scheduler-gated heartbeat to fire many times, which is
/// hundreds of millions to low billions of instructions. Those pass with the
/// budget below (each measured), while the default keeps the fast majority fast.
/// A larger caller `--steps` still wins (`.max`).
fn budget_for(name: &str, default: u64) -> u64 {
    let needed = match name {
        "workload-cooperative-baseline" => 1_000_000_000,
        "frame-allocator-oom" | "heap-oom" => 3_000_000_000,
        "spawn-reclaims-memory" | "spawn-reclaims-names" => 2_500_000_000,
        // `stitch-fs-loads-and-runs` no longer needs an override: `primes(5)`
        // (down from `(10)`) reaches its assertion in ~310M instructions, under
        // the default budget.
        _ => return default,
    };
    needed.max(default)
}

/// Print the per-scenario pass/fail lines, the slowest-scenario table (where the
/// CPU goes), and the headline "N/M pass" summary. `boot_steps` is the one-time
/// per-workload snapshot-boot cost, folded into the honest total-instret figure
/// (the per-scenario steps are post-fork only).
fn print_report(results: &[Row], boot_steps: u64, elapsed_secs: f64) -> ExitCode {
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

    print_slowest(results, boot_steps);

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
fn print_slowest(results: &[Row], boot_steps: u64) {
    const TOP: usize = 12;
    let mut ranked: Vec<&Row> = results.iter().filter(|r| r.steps > 0).collect();
    ranked.sort_unstable_by_key(|r| std::cmp::Reverse(r.steps));
    if ranked.is_empty() {
        return;
    }
    // Total = per-scenario fork-forward instructions + the one-time boot-once
    // cost (paid per workload, not per scenario). The scenario steps are post-fork
    // only, so the boot line makes the north-star figure honest.
    let scenario_total: u64 = ranked.iter().map(|r| r.steps).sum();
    let total = scenario_total + boot_steps;
    println!("\n=== slowest by guest instructions (Minstret; {}M total, {}M boot-once) ===",
        total / 1_000_000, boot_steps / 1_000_000);
    for r in ranked.iter().take(TOP) {
        let pct = if total == 0 { 0.0 } else { 100.0 * r.steps as f64 / total as f64 };
        println!("  {:>7}M  {:>4.1}%  {}", r.steps / 1_000_000, pct, r.name);
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

/// Map `f` over `items` across `jobs` host threads, returning results in the
/// **input order** regardless of the worker count. This is the audit's
/// parallelism primitive: scenarios are independent (each owns its own snemu
/// machine, no shared mutable state), so fanning them out across cores turns the
/// single-threaded compute tail into wall-clock the number of cores divides —
/// while the order-preserving slotting keeps the report deterministic, the
/// property the whole snemu-vs-QEMU story rests on. `jobs <= 1` runs serially.
/// The closure receives `(worker_id, item_index, &item)`: `worker_id` is the host
/// thread (0..jobs) — surfaced so per-item progress can name the CPU thread it ran
/// on — and `item_index` is the item's position in `items`.
fn parallel_map<T, R, F>(items: &[T], jobs: usize, f: F) -> Vec<R>
where
    T: Sync,
    R: Send,
    F: Fn(usize, usize, &T) -> R + Sync,
{
    let jobs = jobs.max(1);
    if jobs == 1 {
        return items.iter().enumerate().map(|(i, t)| f(0, i, t)).collect();
    }

    let next = AtomicUsize::new(0);
    let slots: Mutex<Vec<Option<R>>> =
        Mutex::new((0..items.len()).map(|_| None).collect());

    std::thread::scope(|scope| {
        for worker in 0..jobs {
            let f = &f;
            let next = &next;
            let slots = &slots;
            scope.spawn(move || {
                loop {
                    let i = next.fetch_add(1, Ordering::SeqCst);
                    let Some(item) = items.get(i) else { break };
                    let result = f(worker, i, item);
                    slots.lock().expect("slots mutex poisoned")[i] = Some(result);
                }
            });
        }
    });

    slots
        .into_inner()
        .expect("slots mutex poisoned")
        .into_iter()
        .map(|slot| slot.expect("every slot filled: index range covers all items"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::parallel_map;

    #[test]
    fn parallel_map_preserves_input_order_and_matches_serial_for_any_job_count() {
        let items: Vec<u64> = (0..200).collect();
        let square = |_: usize, _: usize, x: &u64| x * x;

        let serial = parallel_map(&items, 1, square);
        assert_eq!(serial, items.iter().map(|x| x * x).collect::<Vec<_>>());

        for jobs in [2usize, 3, 8, 32] {
            let parallel = parallel_map(&items, jobs, square);
            assert_eq!(
                parallel, serial,
                "jobs={jobs}: results must match serial order and values",
            );
        }
    }

    #[test]
    fn parallel_map_passes_the_input_index_to_the_closure() {
        let items = vec!['a', 'b', 'c'];
        let with_index = parallel_map(&items, 4, |_worker, i, c| (i, *c));
        assert_eq!(with_index, vec![(0, 'a'), (1, 'b'), (2, 'c')]);
    }

    #[test]
    fn parallel_map_reports_a_worker_id_below_the_job_count() {
        let items: Vec<u64> = (0..64).collect();
        let jobs = 4;
        let workers = parallel_map(&items, jobs, |worker, _i, _x| worker);
        assert!(workers.iter().all(|&w| w < jobs), "worker id stays in 0..jobs");
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
