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

// Modules are private; the crate's public surface is exactly the flat
// re-exports below (what `xtask` imports). Internal cross-module access
// goes through `crate::<module>::` paths, which a private `mod` still
// permits. (crate-audit finding #2.)
mod aggregate;
mod baseline;
mod history;
mod lock;
mod metrics;
mod otlp;
mod prom;
mod runner;
mod signature;
mod stats;
#[cfg(test)]
mod test_support;
mod verdict;

// Flat re-exports are exactly what the consumer (`xtask`) imports. Items
// used only inside this crate are reached via their `crate::<module>::`
// path, not re-exported here — keeping the public surface == the actual
// contract. (crate-audit finding #1.)
pub use baseline::{BaselineFile, SummaryOptions};
pub use history::{aggregate_run_dir, prune_runs};
pub use lock::{ItestLock, LockError};
pub use otlp::{push as push_otlp, push_with_timeout as push_otlp_with_timeout};
pub use prom::{render_prometheus, write_atomic};
pub use runner::{CpuProfile, RunnerConfig, Scenario, run};
pub use signature::{CaptureLevel, ErrorOrigin, FailureCapture, WaitOutcome};
