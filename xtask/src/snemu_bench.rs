//! `cargo xtask snemu bench` — the measurement-spine harness. Runs a workload
//! under snemu N times in measurement mode (no per-instruction telemetry, just
//! the timed step loop) and reports guest MIPS + wall-clock spread over a
//! deterministic, identical `instret`. This is the load-bearing "measure first"
//! artifact every later JIT tier is judged against. See
//! `plans/snemu-milestone-4-measurement.md`.

use std::process::ExitCode;

use snemu::bench::{BenchReport, Sample};

use crate::snemu_diff;

/// Run `workload` (or the default `init` boot) under snemu `runs` times to a
/// `max_steps` budget, then print the MIPS/wall-clock report. Determinism is
/// enforced by [`BenchReport::from_samples`]: identical `instret` every run, or
/// it errors loudly.
pub fn run(workload: Option<&str>, max_steps: u64, runs: u32) -> ExitCode {
    if runs == 0 {
        eprintln!("snemu bench: --runs must be at least 1");
        return ExitCode::from(2);
    }
    let (kernel, dtb) = match snemu_diff::prepare(workload.is_some()) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("snemu bench: {e}");
            return ExitCode::from(1);
        }
    };

    let label = workload.unwrap_or("default (init)");
    eprintln!("snemu bench: {label} — {runs} run(s) at up to {max_steps} steps each");

    let mut samples: Vec<Sample> = Vec::with_capacity(runs as usize);
    for i in 0..runs {
        match snemu_diff::measure_workload(&kernel, &dtb, workload, max_steps) {
            Ok(s) => {
                eprintln!(
                    "  run {}/{runs}: {} instr in {:.3}s → {:.2} MIPS",
                    i + 1,
                    s.instret,
                    s.wall.as_secs_f64(),
                    s.mips(),
                );
                samples.push(s);
            }
            Err(e) => {
                eprintln!("snemu bench: {e}");
                return ExitCode::from(1);
            }
        }
    }

    match BenchReport::from_samples(&samples) {
        Ok(r) => {
            print_report(label, &r);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("snemu bench: {e}");
            ExitCode::from(1)
        }
    }
}

fn print_report(label: &str, r: &BenchReport) {
    println!("\n=== snemu bench: {label} ===");
    println!("  instret   {} (deterministic across {} run(s))", r.instret, r.runs);
    println!("  mean wall {:.3}s", r.mean_wall.as_secs_f64());
    println!(
        "  MIPS      best {:.2} / mean {:.2} / worst {:.2}",
        r.best_mips, r.mean_mips, r.worst_mips,
    );
}
