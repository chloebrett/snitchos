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
pub fn run(max_steps: u64, limit: Option<usize>, only: Option<&str>, jobs: usize) -> ExitCode {
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
         up to {max_steps} steps each, {jobs} worker(s)",
        SCENARIOS.len(),
    );

    // Each scenario drives its own live machine: `wait_for` steps it until the
    // frame it needs, `send_input` reaches the modelled UART — so interactive
    // scenarios (console echo, the Stitch REPL) run for real. A passing scenario
    // short-circuits at its last marker; only a failing one runs the full budget.
    // Decode cache is on (verified faithful) so the higher budgets stay cheap.
    //
    // Scenarios are independent — each owns its own machine, no shared mutable
    // state — so they fan out across `jobs` host threads. snemu is a pure
    // interpreter (CPU-bound), so parallelism is worth up to the core count;
    // `parallel_map` keeps the report deterministic by slotting results back into
    // selection order. A `done` counter narrates completions (unordered under
    // fan-out) so a long audit still shows progress.
    let done = AtomicUsize::new(0);
    let started = Instant::now();
    let results: Vec<Row> = parallel_map(&work, jobs, |_, s| {
        let (outcome, steps) = match snemu_diff::load_workload_machine(&kernel, &dtb, s.workload) {
            Ok(machine) => {
                let mut view = View::live(machine, budget_for(s.name, max_steps));
                let outcome = match scenario_view_fn(s.name)(&mut view) {
                    Ok(()) => Outcome::Pass,
                    Err(why) => Outcome::Fail {
                        why,
                        console: console_tail(view.console_output().unwrap_or_default().as_str()),
                    },
                };
                (outcome, view.steps_taken().unwrap_or(0))
            }
            Err(e) => (Outcome::Fail { why: format!("snemu load failed: {e}"), console: String::new() }, 0),
        };
        let n = done.fetch_add(1, Ordering::SeqCst) + 1;
        eprintln!(
            "snemu-itest: [{n}/{cap}] {:<40} {}",
            s.name,
            if matches!(outcome, Outcome::Pass) { "ok" } else { "FAIL" },
        );
        Row { name: s.name, outcome, steps }
    });

    print_report(&results, started.elapsed().as_secs_f64())
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
/// CPU goes), and the headline "N/M pass" summary.
fn print_report(results: &[Row], elapsed_secs: f64) -> ExitCode {
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

    print_slowest(results);

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
fn print_slowest(results: &[Row]) {
    const TOP: usize = 12;
    let mut ranked: Vec<&Row> = results.iter().filter(|r| r.steps > 0).collect();
    ranked.sort_unstable_by_key(|r| std::cmp::Reverse(r.steps));
    if ranked.is_empty() {
        return;
    }
    let total: u64 = ranked.iter().map(|r| r.steps).sum();
    println!("\n=== slowest by guest instructions (Minstret; {}M total) ===", total / 1_000_000);
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
