//! The measurement spine's reporting core: turn a set of timed runs of the
//! *same* deterministic workload into a speed report (guest MIPS + wall-clock
//! spread). Kept pure and host-tested — the `cargo xtask snemu bench` harness
//! does the I/O (load, step, time) and hands the samples here.
//!
//! The load-bearing invariant is **determinism**: snemu retires the exact same
//! instruction count for a given program+seed every run, so every sample must
//! carry an identical `instret`. If they don't, that's a determinism bug in the
//! emulator — [`BenchReport::from_samples`] refuses to average it into a
//! meaningless number and returns an error instead. Only the wall-clock varies
//! run-to-run; MIPS is `instret / wall`, and the *best* (fastest) run is the
//! peak the JIT tiers will be measured against. See
//! `plans/snemu-milestone-4-measurement.md`.

use std::time::Duration;

/// One timed run: how many guest instructions retired and how long it took.
#[derive(Clone, Copy, Debug)]
pub struct Sample {
    pub instret: u64,
    pub wall: Duration,
}

impl Sample {
    /// Guest millions-of-instructions-per-second for this run. `0.0` for a
    /// zero-duration sample (below the clock's resolution) rather than a
    /// division-by-zero infinity.
    #[must_use]
    pub fn mips(&self) -> f64 {
        let secs = self.wall.as_secs_f64();
        if secs <= 0.0 {
            0.0
        } else {
            self.instret as f64 / secs / 1e6
        }
    }
}

/// A speed report over N runs of one deterministic workload: the shared
/// `instret`, and the MIPS mean / best / worst plus the mean wall-clock. Built
/// by [`from_samples`](Self::from_samples), which enforces the determinism
/// invariant.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BenchReport {
    /// The guest instruction count — identical across all samples by
    /// construction (else `from_samples` would have refused them).
    pub instret: u64,
    pub runs: usize,
    pub mean_mips: f64,
    /// From the fastest (shortest wall) run — the peak throughput, the number
    /// a JIT tier's before/after is judged against.
    pub best_mips: f64,
    /// From the slowest run — the spread's other end (scheduler jitter, host
    /// contention).
    pub worst_mips: f64,
    pub mean_wall: Duration,
}

impl BenchReport {
    /// Reduce `samples` to a report, enforcing determinism: every sample must
    /// carry the same `instret`. Returns `Err` on an empty set (nothing to
    /// report) or on any `instret` disagreement (a determinism violation — the
    /// whole premise of engine-independent comparison is broken, so surface it
    /// loudly rather than average past it).
    pub fn from_samples(samples: &[Sample]) -> Result<Self, String> {
        let Some((first, rest)) = samples.split_first() else {
            return Err("no samples to report".to_string());
        };
        let instret = first.instret;
        if let Some(bad) = rest.iter().find(|s| s.instret != instret) {
            return Err(format!(
                "determinism violation: instret varied across runs ({instret} vs {}) — \
                 the same workload+seed must retire the same instruction count",
                bad.instret,
            ));
        }

        let runs = samples.len();
        let mean_mips = samples.iter().map(Sample::mips).sum::<f64>() / runs as f64;
        // Fastest run = shortest wall = highest MIPS; slowest = the opposite.
        let best_mips = samples.iter().map(Sample::mips).fold(f64::MIN, f64::max);
        let worst_mips = samples.iter().map(Sample::mips).fold(f64::MAX, f64::min);
        let mean_wall = samples.iter().map(|s| s.wall).sum::<Duration>() / runs as u32;

        Ok(Self { instret, runs, mean_mips, best_mips, worst_mips, mean_wall })
    }
}

#[cfg(test)]
mod tests {
    use super::{BenchReport, Sample};
    use std::time::Duration;

    fn sample(instret: u64, millis: u64) -> Sample {
        Sample { instret, wall: Duration::from_millis(millis) }
    }

    #[test]
    fn mips_is_instret_over_wall_clock() {
        // 10M instructions in 1s = 10 MIPS.
        let s = sample(10_000_000, 1000);
        assert!((s.mips() - 10.0).abs() < 1e-9);
        // A non-unit duration pins the division the right way round: 10M in 0.5s
        // is 20 MIPS (faster run ⇒ higher MIPS), not 5.
        let faster = sample(10_000_000, 500);
        assert!((faster.mips() - 20.0).abs() < 1e-9);
    }

    #[test]
    fn a_zero_duration_sample_reports_zero_not_infinity() {
        // Below the clock's resolution — report 0, never a divide-by-zero inf.
        let s = sample(1_000_000, 0);
        assert_eq!(s.mips(), 0.0);
    }

    #[test]
    fn a_report_takes_best_from_the_fastest_run() {
        // Same instret, three wall-clocks: the 500ms run is fastest → best MIPS,
        // the 2000ms run slowest → worst. 10M instrs: 20 / 10 / 5 MIPS.
        let samples = [
            sample(10_000_000, 1000),
            sample(10_000_000, 500),
            sample(10_000_000, 2000),
        ];
        let r = BenchReport::from_samples(&samples).expect("uniform instret");
        assert_eq!(r.instret, 10_000_000);
        assert_eq!(r.runs, 3);
        assert!((r.best_mips - 20.0).abs() < 1e-9, "best from the 500ms run");
        assert!((r.worst_mips - 5.0).abs() < 1e-9, "worst from the 2000ms run");
        // mean over the three runs: (20 + 10 + 5) / 3 — pins the divide-by-runs.
        assert!((r.mean_mips - 35.0 / 3.0).abs() < 1e-9, "mean averages all runs");
        // (1000 + 500 + 2000) / 3 ms, exact (Duration division doesn't truncate).
        assert_eq!(r.mean_wall, Duration::from_millis(3500) / 3);
    }

    #[test]
    fn differing_instret_is_a_determinism_violation() {
        // The load-bearing check: if two runs of the same workload retire
        // different instruction counts, that's a determinism bug — refuse to
        // average it into a meaningless MIPS number.
        let samples = [sample(10_000_000, 1000), sample(10_000_042, 1000)];
        let err = BenchReport::from_samples(&samples).expect_err("must reject");
        assert!(err.contains("determinism violation"), "got: {err}");
    }

    #[test]
    fn an_empty_sample_set_is_an_error() {
        assert!(BenchReport::from_samples(&[]).is_err());
    }
}
