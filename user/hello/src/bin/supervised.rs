//! `workload=supervised` — the generic supervisor root (supervision step 2).
//!
//! Where `init`/`supervisor` hardcode *what runs and what to do when it dies*,
//! this program makes that knowledge **data**: a service table it walks. It is
//! the mechanism around the pure `supervision` policy crate — the table, the
//! `WaitAny` loop, and the calls into `startup_order` / `restart_decision`.
//!
//! v1 (this): crash-restart with backoff + an intensity storm-guard. It brings
//! services up in dependency order, reaps whichever exits, and consults the
//! policy: restart (after a backoff), stop, or — once a service crash-loops past
//! its intensity budget — **escalate** (a root escalation is a halt). Cap
//! re-grant on restart (the `satisfier` path) and per-incarnation umbrella spans
//! are steps 3–4; this proves the engine and the escalate path first.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::vec::Vec;

use snitchos_user::{clock_now, entry, exit, register_counter, span_handle, spawn, wait_any, yield_now};
use supervision::{
    ExitOutcome, RestartAction, RestartHistory, RestartLimits, RestartPolicy, ServiceId, ServiceSpec,
    restart_decision, startup_order,
};

/// A supervised service: the pure `ServiceSpec` (id + deps, for ordering) plus
/// the runtime knobs the engine needs to launch and restart it.
struct ServiceDef {
    spec: ServiceSpec,
    /// Human name, for `snitchos.svc.<name>.*` telemetry.
    name: &'static str,
    /// `SPAWNABLE` registry id (Phase 1a: embedded programs by index).
    program: usize,
    /// Delegate our span cap to the child (some programs open a span through it).
    give_span: bool,
    policy: RestartPolicy,
    limits: RestartLimits,
}

/// Live bookkeeping for one service across its incarnations.
struct Live {
    id: ServiceId,
    /// Current incarnation's task id; `None` once the service has stopped.
    task: Option<u32>,
    history: RestartHistory,
}

/// Ids are indices into the service table below, so `deps` reads naturally.
const SPINNER: ServiceId = ServiceId(0);
const CRASHER: ServiceId = ServiceId(1);

/// The service table — the whole "what runs, in what order, restart how" as
/// data. `spinner` is a stable long-lived service; `crasher` (`spawnee`, which
/// exits 42) depends on it and crash-loops, so we watch backoff grow then trip
/// the intensity guard into an escalate.
fn services() -> [ServiceDef; 2] {
    [
        ServiceDef {
            spec: ServiceSpec { id: SPINNER, deps: &[] },
            name: "spinner",
            program: 3,
            give_span: false,
            policy: RestartPolicy::Never,
            limits: NO_RESTART,
        },
        ServiceDef {
            spec: ServiceSpec { id: CRASHER, deps: &[SPINNER] },
            name: "crasher",
            program: 0,
            give_span: true,
            policy: RestartPolicy::OnFailure,
            limits: CRASHER_LIMITS,
        },
    ]
}

const NO_RESTART: RestartLimits =
    RestartLimits { max_restarts: 0, window: 0, backoff_base: 0, backoff_cap: 0 };

/// Small backoff so growth is visible within an itest budget; a wide window so
/// all three restarts count toward intensity and the fourth exit escalates.
const CRASHER_LIMITS: RestartLimits = RestartLimits {
    max_restarts: 3,
    window: 10_000_000_000,
    backoff_base: 100_000,
    backoff_cap: 2_000_000,
};

fn def_for(defs: &[ServiceDef], id: ServiceId) -> &ServiceDef {
    defs.iter().find(|d| d.spec.id == id).expect("id is from the same table")
}

/// Launch one service, delegating its span cap if it wants one. Returns the new
/// incarnation's task id (or escalates the whole supervisor if the spawn fails —
/// a service we can't even start is a fatal supervision error).
fn launch(def: &ServiceDef) -> u32 {
    let handles: &[u32] = if def.give_span { &[span_handle()] } else { &[] };
    match spawn(def.program, handles) {
        Some(task) => task,
        None => escalate(def.name, "spawn-failed"),
    }
}

/// Busy-yield until the monotonic clock reaches `deadline`. No sleep syscall
/// yet, so we spin cooperatively — `yield_now` lets the spinner (and idle/wfi)
/// run while we back off.
fn wait_until(deadline: u64) {
    while clock_now() < deadline {
        yield_now();
    }
}

/// A service exhausted its restart budget (or couldn't be launched). At the root
/// there is no parent to escalate to, so we snitch a fatal event and halt. The
/// `reason` names the escalation path in a span so the trace records *why*.
fn escalate(name: &str, reason: &str) -> ! {
    let _fatal = snitchos_user::tracer().span(&format!("supervised.escalate.{name}.{reason}"));
    register_counter(&format!("snitchos.svc.{name}.escalated")).emit(1);
    register_counter("snitchos.supervised.halted").emit(1);
    exit();
}

#[entry]
fn main() {
    let defs = services();

    // Order the table by dependency; a cycle is a fatal configuration error.
    let specs: Vec<ServiceSpec> = defs.iter().map(|d| d.spec).collect();
    let order = match startup_order(&specs) {
        Ok(order) => order,
        Err(_) => escalate("supervised", "dependency-cycle"),
    };

    // Bring services up in order, recording each incarnation.
    let mut live: Vec<Live> = Vec::with_capacity(order.len());
    for id in &order {
        let task = launch(def_for(&defs, *id));
        live.push(Live { id: *id, task: Some(task), history: RestartHistory { consecutive_failures: 0, recent: Vec::new() } });
    }

    // Supervise. Each reaped child is looked up, its outcome scored, and the
    // policy consulted: restart (after backoff), stop, or escalate.
    loop {
        if live.iter().all(|l| l.task.is_none()) {
            exit();
        }

        let (status, child) = wait_any();
        let now = clock_now();

        let Some(slot) = live.iter_mut().find(|l| l.task == Some(child)) else {
            continue;
        };
        slot.task = None;
        let id = slot.id;
        let def = def_for(&defs, id);

        let outcome = if status == 0 { ExitOutcome::Clean } else { ExitOutcome::Failed(status) };
        match restart_decision(def.policy, outcome, &slot.history, def.limits, now) {
            RestartAction::Stop => {
                register_counter(&format!("snitchos.svc.{}.stopped", def.name)).emit(1);
            }
            RestartAction::Escalate => escalate(def.name, "intensity-exceeded"),
            RestartAction::Restart { after } => {
                // Record this restart for backoff (consecutive failures) and
                // intensity (timestamps within the window), then honor backoff.
                match outcome {
                    ExitOutcome::Failed(_) => slot.history.consecutive_failures += 1,
                    ExitOutcome::Clean => slot.history.consecutive_failures = 0,
                }
                slot.history.recent.push(now);
                let window = def.limits.window;
                slot.history.recent.retain(|t| now.saturating_sub(*t) < window);
                let restarts = slot.history.recent.len() as i64;

                wait_until(now + after);
                let task = launch(def);
                if let Some(slot) = live.iter_mut().find(|l| l.id == id) {
                    slot.task = Some(task);
                }
                register_counter(&format!("snitchos.svc.{}.restarts_total", def.name)).emit(restarts);
            }
        }
    }
}
