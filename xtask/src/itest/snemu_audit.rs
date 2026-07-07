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

use std::collections::BTreeMap;
use std::process::ExitCode;
use std::time::Instant;

use super::harness::View;
use super::{SCENARIOS, scenario_view_fn};
use crate::snemu_diff;

/// One scenario's outcome under the snemu-backed replay.
enum Outcome {
    Pass,
    Fail(String),
}

/// Run the audit: build the `itest-workloads` kernel, boot each distinct
/// workload under snemu to `max_steps`, replay every scenario against its
/// group's frames, and print a per-scenario + summary report. `limit` caps the
/// number of workload groups (faster smoke). Exit is always `SUCCESS` — the
/// audit *reports* fidelity, it doesn't gate on it.
pub fn run(max_steps: u64, limit: Option<usize>) -> ExitCode {
    let (kernel, dtb) = match snemu_diff::prepare(true) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("snemu-itest: {e}");
            return ExitCode::from(1);
        }
    };

    // Group scenarios by their `workload` bootarg so snemu boots each workload
    // once, not once per scenario (the shared-boot idea, applied to the audit).
    // `None` (the default `init` boot) is its own group.
    let mut groups: BTreeMap<Option<&str>, Vec<&str>> = BTreeMap::new();
    for s in SCENARIOS {
        groups.entry(s.workload).or_default().push(s.name);
    }

    let total_groups = groups.len();
    let group_cap = limit.unwrap_or(total_groups).min(total_groups);
    eprintln!(
        "snemu-itest: {} scenarios across {total_groups} workload group(s); \
         auditing {group_cap} group(s) at {max_steps} steps each",
        SCENARIOS.len(),
    );

    let mut results: Vec<(&str, Outcome)> = Vec::new();
    let started = Instant::now();
    for (i, (workload, names)) in groups.iter().take(group_cap).enumerate() {
        let label = workload.unwrap_or("default (init)");
        eprint!("snemu-itest: [{}/{group_cap}] {label:<24} ", i + 1);
        match snemu_diff::collect_workload_frames(&kernel, &dtb, *workload, max_steps) {
            Ok(frames) => {
                eprintln!("({} frames → {} scenario(s))", frames.len(), names.len());
                for name in names {
                    let mut view = View::replay(frames.clone());
                    let outcome = match scenario_view_fn(name)(&mut view) {
                        Ok(()) => Outcome::Pass,
                        Err(e) => Outcome::Fail(e),
                    };
                    results.push((name, outcome));
                }
            }
            // A whole-group boot failure fails every scenario in it — snemu
            // couldn't even produce a stream.
            Err(e) => {
                eprintln!("BOOT ERROR: {e}");
                for name in names {
                    results.push((name, Outcome::Fail(format!("snemu boot failed: {e}"))));
                }
            }
        }
    }

    print_report(&results, started.elapsed().as_secs_f64())
}

/// Print the per-scenario pass/fail lines and the headline "N/M pass" summary.
fn print_report(results: &[(&str, Outcome)], elapsed_secs: f64) -> ExitCode {
    let passed = results.iter().filter(|(_, o)| matches!(o, Outcome::Pass)).count();
    let total = results.len();

    println!("\n=== snemu itest fidelity ===");
    for (name, outcome) in results {
        match outcome {
            Outcome::Pass => println!("  PASS  {name}"),
            Outcome::Fail(why) => println!("  FAIL  {name}\n          {}", first_line(why)),
        }
    }
    println!(
        "\n{passed}/{total} scenarios pass under snemu ({:.0}% fidelity, {elapsed_secs:.1}s)",
        if total == 0 { 0.0 } else { 100.0 * passed as f64 / total as f64 },
    );
    ExitCode::SUCCESS
}

/// A scenario failure message can be multi-line (a dumped frame tail); the
/// report shows only its first line to stay scannable.
fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or(s)
}
