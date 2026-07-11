//! Pure supervision policy — dependency ordering, restart decisions, and
//! restart-set computation. No MMIO, no CSRs, no syscalls: this is the
//! "policy logic" tier that lives beside `sched` and `bootargs` and is
//! exercised entirely by `cargo test -p kernel-core`.
//!
//! The userspace supervisor engine (`workload=supervised-*`) is the
//! mechanism that calls into these decisions; see
//! `docs/supervision-design.md` step 1.

use alloc::vec::Vec;

/// Stable identity for a supervised service, independent of its runtime
/// task id (which changes per incarnation).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ServiceId(pub u32);

/// The supervised-service policy record. Only the fields the pure decisions
/// consume live here today; the engine-facing fields (program ref, needs,
/// readiness) are added as the functions that need them are test-driven.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServiceSpec {
    pub id: ServiceId,
    /// Services that must be started before this one (its upstreams).
    pub deps: &'static [ServiceId],
}

/// Why a service table has no valid startup order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DependencyError {
    /// The `deps` edges contain a cycle; carries the services on it.
    Cycle(Vec<ServiceId>),
}

/// Topologically sort the service table so every service comes after all of
/// its `deps`. Teardown order is the reverse of the returned order.
pub fn startup_order(specs: &[ServiceSpec]) -> Result<Vec<ServiceId>, DependencyError> {
    let mut order = Vec::new();
    let mut done: Vec<ServiceId> = Vec::new();

    while order.len() < specs.len() {
        let ready = specs.iter().find(|s| {
            !done.contains(&s.id) && s.deps.iter().all(|d| done.contains(d))
        });
        let Some(s) = ready else {
            let stuck = specs
                .iter()
                .filter(|s| !done.contains(&s.id))
                .map(|s| s.id)
                .collect();
            return Err(DependencyError::Cycle(stuck));
        };
        order.push(s.id);
        done.push(s.id);
    }
    Ok(order)
}

/// Which services to restart when one dies (Erlang taxonomy, D1). `one-for-all`
/// is deferred until a motivating case exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestartStrategy {
    /// Restart just the dead service.
    OneForOne,
    /// Restart the dead service and everything started after it in `order`
    /// (its dependents), so any cached delegated handle is refreshed.
    RestForOne,
}

/// The set of services to restart, in startup order, given a failure.
pub fn restart_set(
    strategy: RestartStrategy,
    failed: ServiceId,
    order: &[ServiceId],
) -> Vec<ServiceId> {
    match strategy {
        RestartStrategy::OneForOne => alloc::vec![failed],
        RestartStrategy::RestForOne => order
            .iter()
            .skip_while(|s| **s != failed)
            .copied()
            .collect(),
    }
}

/// What to do when a service is restarted at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestartPolicy {
    /// Never restart — a one-shot service.
    Never,
    /// Restart only on a non-zero (failed) exit.
    OnFailure,
    /// Restart on any exit, clean or failed.
    Always,
}

/// How a service's incarnation ended. `Failed` carries the `Wait`/`WaitAny`
/// exit code (0 = clean, non-zero = failure).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitOutcome {
    Clean,
    Failed(i32),
}

/// Restart-storm guard + backoff tuning. All times in raw clock ticks to match
/// `Clock::now`; no `Duration` newtype until the codebase grows one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RestartLimits {
    /// Escalate rather than restart once this many restarts fall inside `window`.
    pub max_restarts: u32,
    /// The intensity window, in ticks.
    pub window: u64,
    /// First-failure backoff, in ticks; doubles per consecutive failure.
    pub backoff_base: u64,
    /// Ceiling on the exponential backoff, in ticks.
    pub backoff_cap: u64,
}

/// Recent restart bookkeeping the decision reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestartHistory {
    /// Consecutive failed restarts, for the exponential backoff exponent.
    pub consecutive_failures: u32,
    /// Timestamps (ticks) of recent restarts, for the intensity check.
    pub recent: Vec<u64>,
}

/// The decision for one exited incarnation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestartAction {
    /// Restart after waiting `after` ticks (0 = immediately).
    Restart { after: u64 },
    /// Do not restart; the service is done.
    Stop,
    /// Restart budget exhausted — hand off to the parent's policy (halt at root).
    Escalate,
}

/// Decide what to do with an exited service incarnation. Pure: a function of
/// the policy, the exit outcome, the restart history, the limits, and the
/// current time.
pub fn restart_decision(
    policy: RestartPolicy,
    outcome: ExitOutcome,
    history: &RestartHistory,
    limits: RestartLimits,
    now: u64,
) -> RestartAction {
    let wants_restart = match policy {
        RestartPolicy::Never => false,
        RestartPolicy::OnFailure => matches!(outcome, ExitOutcome::Failed(_)),
        RestartPolicy::Always => true,
    };
    if !wants_restart {
        return RestartAction::Stop;
    }

    let recent_in_window = history
        .recent
        .iter()
        .filter(|t| now.saturating_sub(**t) < limits.window)
        .count() as u32;
    if recent_in_window >= limits.max_restarts {
        return RestartAction::Escalate;
    }

    let after = match outcome {
        ExitOutcome::Clean => 0,
        ExitOutcome::Failed(_) => limits
            .backoff_base
            .checked_shl(history.consecutive_failures)
            .unwrap_or(limits.backoff_cap)
            .min(limits.backoff_cap),
    };
    RestartAction::Restart { after }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn svc(id: u32, deps: &'static [ServiceId]) -> ServiceSpec {
        ServiceSpec { id: ServiceId(id), deps }
    }

    #[test]
    fn linear_chain_orders_dependencies_first() {
        // c depends on b depends on a → a must come before b before c.
        let specs = [
            svc(2, &[ServiceId(1)]),
            svc(3, &[ServiceId(2)]),
            svc(1, &[]),
        ];
        assert_eq!(
            startup_order(&specs),
            Ok(alloc::vec![ServiceId(1), ServiceId(2), ServiceId(3)])
        );
    }

    #[test]
    fn diamond_places_shared_dependency_once_before_dependents() {
        // b and c both depend on a; d depends on both. a first, d last, and
        // a appears exactly once.
        let specs = [
            svc(1, &[]),
            svc(2, &[ServiceId(1)]),
            svc(3, &[ServiceId(1)]),
            svc(4, &[ServiceId(2), ServiceId(3)]),
        ];
        let order = startup_order(&specs).expect("acyclic");
        let pos = |id: u32| order.iter().position(|s| *s == ServiceId(id)).unwrap();
        assert_eq!(order.len(), 4);
        assert!(pos(1) < pos(2) && pos(1) < pos(3));
        assert!(pos(2) < pos(4) && pos(3) < pos(4));
    }

    #[test]
    fn cycle_is_reported_with_the_offending_services() {
        // a → b → a is unsatisfiable; both land in the Cycle report.
        let specs = [
            svc(1, &[ServiceId(2)]),
            svc(2, &[ServiceId(1)]),
        ];
        match startup_order(&specs) {
            Err(DependencyError::Cycle(nodes)) => {
                assert!(nodes.contains(&ServiceId(1)));
                assert!(nodes.contains(&ServiceId(2)));
            }
            other => panic!("expected Cycle, got {other:?}"),
        }
    }

    fn limits() -> RestartLimits {
        RestartLimits { max_restarts: 3, window: 1000, backoff_base: 10, backoff_cap: 80 }
    }

    fn history(consecutive_failures: u32, recent: &'static [u64]) -> RestartHistory {
        RestartHistory { consecutive_failures, recent: recent.into() }
    }

    #[test]
    fn never_policy_stops_on_any_outcome() {
        let h = history(0, &[]);
        assert_eq!(
            restart_decision(RestartPolicy::Never, ExitOutcome::Clean, &h, limits(), 0),
            RestartAction::Stop
        );
        assert_eq!(
            restart_decision(RestartPolicy::Never, ExitOutcome::Failed(1), &h, limits(), 0),
            RestartAction::Stop
        );
    }

    #[test]
    fn on_failure_policy_stops_on_clean_and_restarts_on_failure() {
        let h = history(0, &[]);
        assert_eq!(
            restart_decision(RestartPolicy::OnFailure, ExitOutcome::Clean, &h, limits(), 0),
            RestartAction::Stop
        );
        assert_eq!(
            restart_decision(RestartPolicy::OnFailure, ExitOutcome::Failed(1), &h, limits(), 0),
            RestartAction::Restart { after: 10 }
        );
    }

    #[test]
    fn always_policy_restarts_a_clean_exit_with_no_backoff() {
        // A clean exit under `Always` isn't a failure loop — restart immediately.
        let h = history(0, &[]);
        assert_eq!(
            restart_decision(RestartPolicy::Always, ExitOutcome::Clean, &h, limits(), 0),
            RestartAction::Restart { after: 0 }
        );
    }

    #[test]
    fn failure_backoff_doubles_with_consecutive_failures() {
        // base 10 → 10, 20, 40 as consecutive failures climb.
        assert_eq!(
            restart_decision(RestartPolicy::OnFailure, ExitOutcome::Failed(1), &history(1, &[]), limits(), 0),
            RestartAction::Restart { after: 20 }
        );
        assert_eq!(
            restart_decision(RestartPolicy::OnFailure, ExitOutcome::Failed(1), &history(2, &[]), limits(), 0),
            RestartAction::Restart { after: 40 }
        );
    }

    #[test]
    fn failure_backoff_saturates_at_the_cap() {
        // base 10 · 2^4 = 160 > cap 80 → clamp to 80; a huge exponent must not
        // overflow the shift, either.
        assert_eq!(
            restart_decision(RestartPolicy::OnFailure, ExitOutcome::Failed(1), &history(4, &[]), limits(), 0),
            RestartAction::Restart { after: 80 }
        );
        assert_eq!(
            restart_decision(RestartPolicy::OnFailure, ExitOutcome::Failed(1), &history(99, &[]), limits(), 0),
            RestartAction::Restart { after: 80 }
        );
    }

    #[test]
    fn intensity_breach_escalates_instead_of_restarting() {
        // max_restarts 3, window 1000: three restarts inside the window and the
        // next exit escalates rather than looping.
        let h = history(3, &[100, 200, 300]);
        assert_eq!(
            restart_decision(RestartPolicy::OnFailure, ExitOutcome::Failed(1), &h, limits(), 400),
            RestartAction::Escalate
        );
    }

    #[test]
    fn a_restart_exactly_one_window_old_has_just_aged_out() {
        // Boundary: distance == window is *outside* the window. With now=1000
        // and window=1000, the restart at t=0 must not count, leaving 2 (< 3)
        // inside → a normal restart, not an escalation.
        let h = history(2, &[0, 100, 200]);
        assert_eq!(
            restart_decision(RestartPolicy::OnFailure, ExitOutcome::Failed(1), &h, limits(), 1000),
            RestartAction::Restart { after: 40 }
        );
    }

    #[test]
    fn restarts_outside_the_window_do_not_count_toward_intensity() {
        // Same three restarts, but now is far enough out that they've aged past
        // the 1000-tick window → back to a normal backoff restart.
        let h = history(3, &[100, 200, 300]);
        assert_eq!(
            restart_decision(RestartPolicy::OnFailure, ExitOutcome::Failed(1), &h, limits(), 2000),
            RestartAction::Restart { after: 80 }
        );
    }

    #[test]
    fn one_for_one_restarts_only_the_failed_service() {
        let order = [ServiceId(1), ServiceId(2), ServiceId(3)];
        assert_eq!(
            restart_set(RestartStrategy::OneForOne, ServiceId(2), &order),
            alloc::vec![ServiceId(2)]
        );
    }

    #[test]
    fn rest_for_one_restarts_the_failed_service_and_its_downstreams() {
        // Everything started after the failed one (its dependents in start
        // order) restarts too, so any cached delegated handle is refreshed.
        let order = [ServiceId(1), ServiceId(2), ServiceId(3)];
        assert_eq!(
            restart_set(RestartStrategy::RestForOne, ServiceId(2), &order),
            alloc::vec![ServiceId(2), ServiceId(3)]
        );
    }

    #[test]
    fn rest_for_one_on_the_last_service_restarts_only_it() {
        let order = [ServiceId(1), ServiceId(2), ServiceId(3)];
        assert_eq!(
            restart_set(RestartStrategy::RestForOne, ServiceId(3), &order),
            alloc::vec![ServiceId(3)]
        );
    }
}
