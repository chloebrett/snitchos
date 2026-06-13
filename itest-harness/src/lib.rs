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
pub use runner::{CpuProfile, RunnerConfig, Scenario, run, select_by_tags};
pub use signature::{CaptureLevel, ErrorOrigin, FailureCapture, WaitOutcome};

/// Build a `&[Scenario]` catalog as a table instead of a wall of
/// `Scenario::new(...).tagged(...)` calls. One row per scenario:
///
/// ```ignore
/// const SCENARIOS: &[Scenario] = scenarios! {
///     wfi "boot-reaches-heartbeat" scenarios::boot_reaches_heartbeat;
///     cpu "spawn-storm"            scenarios::spawn_storm            [smp, stress];
///     wfi "userspace-emits-span"   scenarios::userspace_emits_span   [userspace] {"userspace"};
/// };
/// ```
///
/// Row grammar: `<profile> <name-literal> <fn-path> [tag, …]? {<workload-lit>}? ;`
/// - **profile** — `wfi` maps to [`Scenario::new`], `cpu` to [`Scenario::cpu_bound`].
/// - **tags** — optional `[ident, …]`; bare idents become the string tags
///   (`[smp, stress]` → `.tagged(&["smp", "stress"])`). Must be valid
///   identifiers — single words, no hyphens.
/// - **workload** — optional `{"<name>"}`; the runtime workload the scenario
///   boots (`.on_workload("<name>")`), and the grouping key for shared-boot
///   mode. Omit for the default demo. (Braced rather than a `workload`
///   keyword because a bare ident can't follow a `:path` fragment in a
///   matcher, but `{` can.)
#[macro_export]
macro_rules! scenarios {
    // Entry point: a `;`-separated list of rows, optional trailing `;`.
    // Each row expands to a base `Scenario` plus the optional `.tagged(…)`
    // / `.on_workload(…)` modifiers, applied only when present.
    ( $(
        $profile:ident $name:literal $func:path
        $( [ $( $tag:ident ),* $(,)? ] )?
        $( { $wl:literal } )?
    );* $(;)? ) => {
        &[ $(
            $crate::scenarios!(@base $profile $name $func)
                $( .tagged(&[ $( stringify!($tag) ),* ]) )?
                $( .on_workload($wl) )?
        ),* ]
    };

    // Base constructor, dispatched on the profile keyword.
    (@base wfi $name:literal $func:path) => { $crate::Scenario::new($name, $func) };
    (@base cpu $name:literal $func:path) => { $crate::Scenario::cpu_bound($name, $func) };
}
