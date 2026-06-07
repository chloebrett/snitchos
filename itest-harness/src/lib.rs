//! Host-buildable integration-test harness.
//!
//! Owns the platform-pure runner mechanics: `--repeat N` aggregation,
//! per-scenario flake-rate tracking, statistical framing, baseline-file
//! regression detection, log-dump-on-failure, lifecycle hooks. No
//! dependency on what's under test — consumers plug in their own
//! `Subject` (process to launch + event stream to decode).
//!
//! See `plans/itest-harness-extraction.md` for the migration plan and
//! the broader rationale.

pub mod aggregate;
pub mod baseline;
pub mod stats;

pub use aggregate::{Aggregator, RunTotals};
pub use baseline::{Baseline, BaselineError, BaselineFile, ScenarioBaseline};
pub use stats::{ConfidenceInterval, two_proportion_p_value, wilson_score_95};
