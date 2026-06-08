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
pub mod otlp;
pub mod prom;
pub mod runner;
pub mod stats;
pub mod verdict;

pub use aggregate::{Aggregator, RunTotals};
pub use baseline::{
    Baseline, BaselineError, BaselineFile, PartialMarker, ScenarioBaseline, SummaryOptions,
};
pub use history::{
    PruneReport, RecoveredRun, RecoveredScenario, aggregate_run_dir, prune_runs,
};
pub use lock::{ItestLock, LockError};
pub use otlp::{
    build_payload as build_otlp_payload, metrics_endpoint, push as push_otlp,
    push_with_timeout as push_otlp_with_timeout,
};
pub use prom::{render_prometheus, write_atomic};
pub use runner::{RunnerConfig, Scenario, run};
pub use stats::{ConfidenceInterval, two_proportion_p_value, wilson_score_95};
pub use verdict::{
    ComparisonRender, DEFAULT_ALPHA, Direction, Verdict, render_comparison, verdict,
};
