//! Shared test fixtures. `#[cfg(test)]`-only — compiled out of normal
//! builds. Factory functions live here when more than one module's tests
//! need the same shape, per the project's "factory functions for test
//! data" convention.

use crate::baseline::Baseline;
use time::macros::datetime;

/// A `current` baseline with the given `runs`/`failures` and
/// representative timing. Shared by the two exporter test modules (`prom`
/// and `otlp`), which both need the duration metrics populated. Other
/// modules' baselines differ enough (no durations, or a parameterised
/// commit/timestamp) to keep their own local builders.
pub(crate) fn baseline_with(runs: u32, failures: u32) -> Baseline {
    Baseline {
        commit: "abc1234".to_string(),
        build_hash: None,
        runs,
        failures,
        recorded_at: datetime!(2026-06-08 12:00:00 UTC),
        mean_duration_ms: Some(1200.0),
        p95_duration_ms: Some(1500.0),
        partial: None,
        signature_counts: Default::default(),
    }
}
