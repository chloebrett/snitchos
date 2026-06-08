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
pub mod history;
pub mod lock;
pub mod runner;
pub mod stats;
pub mod verdict;

pub use aggregate::{Aggregator, RunTotals};
pub use baseline::{
    Baseline, BaselineError, BaselineFile, PartialMarker, ScenarioBaseline, SummaryOptions,
};
pub use lock::{ItestLock, LockError};
pub use runner::{RunnerConfig, Scenario, run};
pub use stats::{ConfidenceInterval, two_proportion_p_value, wilson_score_95};
pub use verdict::{
    ComparisonRender, DEFAULT_ALPHA, Direction, Verdict, render_comparison, verdict,
};
