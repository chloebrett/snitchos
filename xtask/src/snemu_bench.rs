//! `cargo xtask snemu bench` — the measurement-spine harness. Runs a workload
//! under snemu N times in measurement mode (no per-instruction telemetry, just
//! the timed step loop) and reports guest MIPS + wall-clock spread over a
//! deterministic, identical `instret`. This is the load-bearing "measure first"
//! artifact every later JIT tier is judged against. See
//! `plans/snemu-milestone-4-measurement.md`.

use std::process::ExitCode;

use snemu::bench::{BenchReport, Sample};

use crate::snemu_diff;

/// One workload class in the measurement taxonomy: a representative runtime
/// workload run to a fixed instruction budget. The four classes give "various
/// workloads" texture — the interpreter's per-instruction cost varies with the
/// instruction mix, so MIPS differs class-to-class, and each JIT tier attacks a
/// different bar. See `plans/snemu-milestone-4-measurement.md` step 3.
pub struct TaxonomyEntry {
    pub class: &'static str,
    pub workload: &'static str,
    pub steps: u64,
    pub why: &'static str,
}

/// The four checked-in taxonomy benchmarks. Each maps a class to a real
/// runtime workload (validated against the registry in tests) plus a fixed
/// step budget, so cross-engine comparison is exact (snemu's determinism makes
/// the same budget retire the same instret every run).
pub const TAXONOMY: &[TaxonomyEntry] = &[
    TaxonomyEntry {
        class: "startup-bound",
        workload: "demo",
        steps: 10_000_000,
        why: "boot + first heartbeats — the MMIO handshake and page-table setup dominate",
    },
    TaxonomyEntry {
        class: "compute-bound",
        workload: "mutex-storm",
        steps: 50_000_000,
        why: "a tight lock-acquire loop — mostly register and atomic ops",
    },
    TaxonomyEntry {
        class: "memory-bound",
        workload: "heap-oom",
        steps: 50_000_000,
        why: "allocator churn (16 MiB/tick) — load/store heavy through the soft-MMU",
    },
    TaxonomyEntry {
        class: "trap-mmio-heavy",
        workload: "syscall-hog",
        steps: 50_000_000,
        why: "a user task spamming syscalls — trap crossings and CSR work",
    },
];

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

    match bench_one(&kernel, &dtb, workload, max_steps, runs, true) {
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

/// Sweep the four taxonomy classes ([`TAXONOMY`]), each at its own fixed budget,
/// and print a one-row-per-class comparison table. The "various workloads"
/// picture: MIPS varies with the instruction mix, and each row is a bar a JIT
/// tier will try to move.
pub fn run_taxonomy(runs: u32) -> ExitCode {
    if runs == 0 {
        eprintln!("snemu bench: --runs must be at least 1");
        return ExitCode::from(2);
    }
    // Every taxonomy workload is a runtime-selected workload, so the build needs
    // the `itest-workloads` feature.
    let (kernel, dtb) = match snemu_diff::prepare(true) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("snemu bench: {e}");
            return ExitCode::from(1);
        }
    };

    let mut rows: Vec<(&TaxonomyEntry, BenchReport)> = Vec::new();
    for e in TAXONOMY {
        eprintln!(
            "snemu bench: {:<16} {} — {runs} run(s) at up to {} steps",
            e.class, e.workload, e.steps,
        );
        match bench_one(&kernel, &dtb, Some(e.workload), e.steps, runs, false) {
            Ok(r) => rows.push((e, r)),
            Err(err) => {
                eprintln!("snemu bench: {} ({}): {err}", e.class, e.workload);
                return ExitCode::from(1);
            }
        }
    }

    print_taxonomy(&rows);
    ExitCode::SUCCESS
}

/// Run `workload` `runs` times to `steps`, optionally logging each run, and
/// reduce to a determinism-checked [`BenchReport`]. Shared by the single-
/// workload and taxonomy paths.
fn bench_one(
    kernel: &[u8],
    dtb: &[u8],
    workload: Option<&str>,
    steps: u64,
    runs: u32,
    verbose: bool,
) -> Result<BenchReport, String> {
    let mut samples: Vec<Sample> = Vec::with_capacity(runs as usize);
    for i in 0..runs {
        let s = snemu_diff::measure_workload(kernel, dtb, workload, steps)?;
        if verbose {
            let startup = s.startup.map_or_else(
                || " (silent)".to_string(),
                |m| format!(", startup {} instr / {:.3}s", m.instret, m.wall.as_secs_f64()),
            );
            eprintln!(
                "  run {}/{runs}: {} instr in {:.3}s → {:.2} MIPS{startup}",
                i + 1,
                s.instret,
                s.wall.as_secs_f64(),
                s.mips(),
            );
        }
        samples.push(s);
    }
    BenchReport::from_samples(&samples)
}

/// Print the taxonomy comparison table: one row per class with its best MIPS,
/// mean wall, and startup instret. The trailing note explains why each class
/// stresses the interpreter differently.
fn print_taxonomy(rows: &[(&TaxonomyEntry, BenchReport)]) {
    println!("\n=== snemu bench: taxonomy ===");
    println!(
        "  {:<16} {:<14} {:>12} {:>10} {:>10}  startup",
        "CLASS", "WORKLOAD", "INSTRET", "MIPS(best)", "wall(s)",
    );
    for (e, r) in rows {
        let startup = r
            .startup_instret
            .map_or_else(|| "—".to_string(), |i| i.to_string());
        println!(
            "  {:<16} {:<14} {:>12} {:>10.2} {:>10.3}  {startup}",
            e.class,
            e.workload,
            r.instret,
            r.best_mips,
            r.mean_wall.as_secs_f64(),
        );
    }
    println!();
    for (e, _) in rows {
        println!("  {:<16} {}", e.class, e.why);
    }
}

#[cfg(test)]
mod taxonomy_tests {
    use super::TAXONOMY;
    use crate::snemu_diff::WORKLOADS;
    use std::collections::HashSet;

    #[test]
    fn the_taxonomy_covers_the_four_canonical_classes() {
        // The plan's four workload classes must each be a checked-in benchmark,
        // exactly once — a missing or duplicated class would skew the "various
        // workloads" story.
        let classes: HashSet<&str> = TAXONOMY.iter().map(|e| e.class).collect();
        assert_eq!(classes.len(), TAXONOMY.len(), "class names must be unique");
        assert_eq!(
            classes,
            HashSet::from(["startup-bound", "compute-bound", "memory-bound", "trap-mmio-heavy"]),
        );
    }

    #[test]
    fn every_taxonomy_workload_is_a_real_registered_workload() {
        // A typo'd workload name would silently boot the wrong (or no) scenario;
        // pin each entry to the actual runtime-workload registry.
        for e in TAXONOMY {
            assert!(
                WORKLOADS.contains(&e.workload),
                "taxonomy workload {:?} is not a registered workload",
                e.workload,
            );
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
    match (r.startup_instret, r.mean_startup_wall) {
        (Some(instret), Some(wall)) => println!(
            "  startup   {instret} instr to first telemetry / {:.3}s mean wall",
            wall.as_secs_f64(),
        ),
        _ => println!("  startup   — (no telemetry within budget)"),
    }
}
