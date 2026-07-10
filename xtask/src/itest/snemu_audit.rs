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

use std::process::ExitCode;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

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
) -> ExitCode {
    let (kernel, dtb) = match snemu_diff::prepare(true) {
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

    // Boot-once, forked per scenario. Every scenario on a workload shares the same
    // boot-to-steady-state, so a workload is booted **once** to the `entering
    // heartbeat` checkpoint and snapshotted (`Machine: Clone` — the deep copy the
    // machine was built for); each of its scenarios then forks the snapshot rather
    // than re-running the ~25M-instruction boot. For 108 scenarios over ~30
    // workloads that removes ~2B instructions of redundant boot. Fidelity-exact:
    // the clone carries machine state *and* the emitted-frame history (its
    // virtio-TX buffer), so even boot-time-assertion scenarios see their frames.
    //
    // The unit of parallelism is the **workload group**, not the scenario: a
    // `Machine` holds a `RefCell` (single-thread by design), so it must never
    // cross a thread boundary. One worker owns a group's snapshot and forks it
    // locally for each scenario. A dynamic work-queue (`parallel_map`) balances
    // groups across cores; the heavy scenarios are mostly singleton workloads, so
    // they still fan out. A workload that never reaches the checkpoint (an early
    // crash) falls back to a fresh boot per scenario.
    let groups = group_by_workload(&work);
    let done = AtomicUsize::new(0);
    let group_results = parallel_map(&groups, jobs, |_, group| {
        run_group(&kernel, &dtb, group, max_steps, idle_skip, &done, cap)
    });

    // Flatten groups back into selection order via each scenario's original index.
    let mut rows: Vec<Option<Row>> = (0..work.len()).map(|_| None).collect();
    let mut boot_steps = 0u64;
    for (group_boot, scenario_rows) in group_results {
        boot_steps += group_boot;
        for (index, row) in scenario_rows {
            rows[index] = Some(row);
        }
    }
    let results: Vec<Row> = rows.into_iter().flatten().collect();

    print_report(&results, boot_steps, started.elapsed().as_secs_f64())
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

/// One workload's scenarios, tagged with their original position in the selection
/// (so results can be slotted back into report order after the parallel fan-out).
struct Group<'a> {
    workload: Option<&'a str>,
    scenarios: Vec<(usize, &'a itest_harness::Scenario)>,
}

/// Partition `work` into per-workload groups, preserving first-appearance order of
/// workloads and selection order within each. Each carries its scenarios' original
/// indices for the post-fan-out re-ordering.
fn group_by_workload<'a>(work: &[&'a itest_harness::Scenario]) -> Vec<Group<'a>> {
    let mut groups: Vec<Group<'a>> = Vec::new();
    for (index, s) in work.iter().enumerate() {
        match groups.iter_mut().find(|g| g.workload == s.workload) {
            Some(g) => g.scenarios.push((index, s)),
            None => groups.push(Group { workload: s.workload, scenarios: vec![(index, s)] }),
        }
    }
    groups
}

/// Run one workload group on a single thread: boot its snapshot once to the
/// checkpoint, then fork (clone) it for each scenario. Returns the one-time boot
/// cost and each scenario's `(original_index, Row)`. If the snapshot never reaches
/// the checkpoint (early-crash workload), every scenario boots fresh instead.
fn run_group(
    kernel: &[u8],
    dtb: &[u8],
    group: &Group,
    max_steps: u64,
    idle_skip: bool,
    done: &AtomicUsize,
    cap: usize,
) -> (u64, Vec<(usize, Row)>) {
    let snapshot = boot_snapshot(kernel, dtb, group.workload, idle_skip);
    let boot_steps = snapshot.as_ref().map_or(0, |(_, n)| *n);

    let rows = group
        .scenarios
        .iter()
        .map(|&(index, s)| {
            let (outcome, steps) = match &snapshot {
                // Fork the shared post-boot snapshot (idle-skip already set on it).
                Ok((machine, _)) => run_scenario(machine.clone(), s, max_steps),
                // Never reached the checkpoint: boot this scenario fresh.
                Err(_) => match snemu_diff::load_workload_machine(kernel, dtb, s.workload) {
                    Ok(mut machine) => {
                        machine.set_idle_skip(idle_skip);
                        run_scenario(machine, s, max_steps)
                    }
                    Err(e) => (
                        Outcome::Fail { why: format!("snemu load failed: {e}"), console: String::new() },
                        0,
                    ),
                },
            };
            let n = done.fetch_add(1, Ordering::SeqCst) + 1;
            eprintln!(
                "snemu-itest: [{n}/{cap}] {:<40} {}",
                s.name,
                if matches!(outcome, Outcome::Pass) { "ok" } else { "FAIL" },
            );
            (index, Row { name: s.name, outcome, steps })
        })
        .collect();

    (boot_steps, rows)
}

/// Boot a fresh machine for `workload` to [`CHECKPOINT`] with idle-skip set — the
/// snapshot every scenario in the group forks. `Err` if it never reached the
/// checkpoint (the caller then boots each scenario fresh).
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
fn parallel_map<T, R, F>(items: &[T], jobs: usize, f: F) -> Vec<R>
where
    T: Sync,
    R: Send,
    F: Fn(usize, &T) -> R + Sync,
{
    let jobs = jobs.max(1);
    if jobs == 1 {
        return items.iter().enumerate().map(|(i, t)| f(i, t)).collect();
    }

    let next = AtomicUsize::new(0);
    let slots: Mutex<Vec<Option<R>>> =
        Mutex::new((0..items.len()).map(|_| None).collect());

    std::thread::scope(|scope| {
        for _ in 0..jobs {
            scope.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::SeqCst);
                    let Some(item) = items.get(i) else { break };
                    let result = f(i, item);
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
        let square = |_: usize, x: &u64| x * x;

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
        let with_index = parallel_map(&items, 4, |i, c| (i, *c));
        assert_eq!(with_index, vec![(0, 'a'), (1, 'b'), (2, 'c')]);
    }
}
