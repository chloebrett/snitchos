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
//! its intensity budget — **escalate** (a root escalation is a halt).
//!
//! Telemetry (step 3): each transition drives a `snitchos.svc.<name>.state` gauge
//! (Starting/Running/Backoff/Stopped/Escalated), plus `.restarts_total`,
//! `.backoff_ticks`, and per-incarnation `.uptime_ticks`, and point-event spans at
//! exit/escalate. **Deferred:** the long-lived *umbrella span per service with a
//! child span per incarnation* (the Tempo trace tree) — the kernel span cursor is
//! per-task LIFO, so a single supervisor task can't hold concurrent per-service
//! spans open across the `WaitAny` loop; that tree needs an explicit-parent span
//! model.
//!
//! Cap re-grant (step 4, D3): the supervisor owns a durable endpoint (`svc-ep`) and
//! is a **manifest satisfier** — each incarnation it runs `hitch::satisfy(needs,
//! have)` (the shared primitive `satisfier.rs` and checkpoint use) against its own
//! caps and delegates the plan in slot order, minting an attenuated `SEND` for
//! `crasher`'s declared need. On restart, the same `satisfy` re-runs against the new
//! `CapTable`. The `cap-reporter` program then enumerates its own `cap_list` and
//! emits `snitchos.reporter.holds_endpoint` — the holder's independent confirmation
//! that the re-granted cap landed (the snitch-on-the-snitch oracle for D3).

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::vec::Vec;

use hitch::{CapView, Grant, Slot, satisfy};
use snitchos_user::{
    Endpoint, Metric, clock_now, endpoint_create, entry, exit, object_kind, register_counter,
    register_gauge, rights, spawn, tracer, wait_any, yield_now,
};
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
    /// The child's declared authority requirements (a manifest `needs` list). The
    /// supervisor is a *satisfier*: each incarnation it runs `hitch::satisfy(needs,
    /// have)` against its own caps and delegates the result — so cap re-grant on
    /// restart is the shared, data-driven `satisfy` primitive, not a hand-rolled
    /// mint. (Here `needs` is static per service; the FS-manifest source the
    /// `satisfier` reads from a `user.iface` xattr is a separate axis.)
    needs: Vec<Slot>,
    policy: RestartPolicy,
    limits: RestartLimits,
}

/// A service's metric handles, **registered once** at bring-up and reused for the
/// life of the process. The per-process metric table is bounded (16 slots) and
/// registration does not dedup by name, so re-registering per emit inside the
/// supervise loop would exhaust it — the late `escalate` counters would then be
/// refused. Register once, emit through the handle (`Metric` is `Copy`).
#[derive(Clone, Copy)]
struct Metrics {
    state: Metric,
    restarts: Metric,
    backoff: Metric,
    uptime: Metric,
}

fn register_metrics(name: &str) -> Metrics {
    Metrics {
        state: register_gauge(&format!("snitchos.svc.{name}.state")),
        restarts: register_counter(&format!("snitchos.svc.{name}.restarts_total")),
        backoff: register_gauge(&format!("snitchos.svc.{name}.backoff_ticks")),
        uptime: register_gauge(&format!("snitchos.svc.{name}.uptime_ticks")),
    }
}

/// Live bookkeeping for one service across its incarnations.
struct Live {
    id: ServiceId,
    /// Current incarnation's task id; `None` once the service has stopped.
    task: Option<u32>,
    /// When the current incarnation last became `Running` — for `uptime_ticks`.
    started: u64,
    /// Pre-registered metric handles (see [`Metrics`]).
    metrics: Metrics,
    history: RestartHistory,
}

/// The lifecycle state a service is in, emitted as the `snitchos.svc.<name>.state`
/// gauge so Grafana can render a per-service state timeline (and a crash loop shows
/// as `Running`↔`Backoff` flapping before it trips `Escalated`). Values are stable;
/// the collector maps them back to names.
#[derive(Clone, Copy)]
#[repr(i64)]
enum State {
    Starting = 1,
    Running = 2,
    Backoff = 3,
    Stopped = 4,
    Escalated = 5,
}

/// Ids are indices into the service table below, so `deps` reads naturally.
const SPINNER: ServiceId = ServiceId(0);
const CRASHER: ServiceId = ServiceId(1);

/// The service table — the whole "what runs, in what order, restart how" as
/// data. `spinner` is a stable long-lived service; `crasher` (the `cap-reporter`
/// program, exit 17) depends on it, holds a re-granted endpoint each incarnation,
/// and crash-loops — so we watch cap re-grant + backoff before intensity escalates.
fn services() -> [ServiceDef; 2] {
    [
        ServiceDef {
            spec: ServiceSpec { id: SPINNER, deps: &[] },
            name: "spinner",
            program: 3,
            needs: Vec::new(),
            policy: RestartPolicy::Never,
            limits: NO_RESTART,
        },
        ServiceDef {
            // `cap-reporter` (SPAWNABLE id 7): reads its own cap_list, reports
            // whether the re-granted endpoint landed, then crashes (exit 17). It
            // *declares* it needs a `SEND` on the `svc-ep` endpoint; the supervisor
            // satisfies that from its own `RECV | MINT` by minting an attenuated cap.
            spec: ServiceSpec { id: CRASHER, deps: &[SPINNER] },
            name: "crasher",
            program: 7,
            needs: alloc::vec![Slot {
                name: "svc-ep".into(),
                object: object_kind::ENDPOINT as u8,
                rights: rights::SEND,
            }],
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

/// Launch one service, **satisfying its declared `needs`** from the supervisor's
/// own caps (`hitch::satisfy`) and delegating the plan in slot order — the shared,
/// data-driven re-grant primitive, re-run per incarnation. Returns the new
/// incarnation's task id, or escalates the whole supervisor if a slot is
/// unsatisfiable, a mint fails, or the spawn fails (a service we can't grant
/// authority to — or even start — is a fatal supervision error).
fn launch(def: &ServiceDef, ep: &Endpoint) -> u32 {
    // What we can satisfy from: our durable endpoint. We advertise the rights we
    // can *provide*, not just the ones the cap literally carries — holding `MINT`
    // means we can mint any right (`SEND`/`RECV`) for a child, so `satisfy` sees
    // them as grantable (it matches on advertised rights and mints the attenuation).
    // Same pattern as `satisfier.rs`, which advertises `MINT | SEND`.
    let have = [CapView {
        object: object_kind::ENDPOINT as u8,
        rights: rights::RECV | rights::SEND | rights::MINT,
        handle: ep.raw_handle() as u32,
    }];
    let plan = match satisfy(&def.needs, &have) {
        Ok(plan) => plan,
        Err(_) => escalate(def.name, "unsatisfiable-needs"),
    };

    // Assemble the delegated-handle array in slot order: an exact match rides as-is,
    // a wider held cap is attenuated by minting exactly the slot's rights.
    let mut handles: Vec<u32> = Vec::with_capacity(plan.len());
    for grant in &plan {
        let handle = match grant {
            Grant::Use { handle } => *handle,
            Grant::Mint { from, rights } => {
                match Endpoint::from_raw_handle(*from as usize).mint_badged(0, *rights) {
                    Ok(h) => h as u32,
                    Err(_) => escalate(def.name, "mint-failed"),
                }
            }
        };
        handles.push(handle);
    }

    match spawn(def.program, &handles) {
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
    // Terminal path — runs once, so inline (one-shot) registration is safe; it
    // won't leak metric-table slots the way a per-emit loop would.
    let _fatal = tracer().span(&format!("supervised.escalate.{name}.{reason}"));
    register_gauge(&format!("snitchos.svc.{name}.state")).emit(State::Escalated as i64);
    register_counter(&format!("snitchos.svc.{name}.escalated")).emit(1);
    register_counter("snitchos.supervised.halted").emit(1);
    exit();
}

#[entry]
fn main() {
    let defs = services();

    // The supervisor owns the durable endpoint (D3): it lives across every service
    // incarnation, so a service is restartable *because* its authority is ours to
    // re-grant. Named so the reporter can confirm it by object name in `cap_list`.
    let ep = endpoint_create("svc-ep");

    // Order the table by dependency; a cycle is a fatal configuration error.
    let specs: Vec<ServiceSpec> = defs.iter().map(|d| d.spec).collect();
    let order = match startup_order(&specs) {
        Ok(order) => order,
        Err(_) => escalate("supervised", "dependency-cycle"),
    };

    // Bring services up in order, recording each incarnation and its state.
    // Register each service's metrics once here and reuse the handles.
    let mut live: Vec<Live> = Vec::with_capacity(order.len());
    for id in &order {
        let def = def_for(&defs, *id);
        let metrics = register_metrics(def.name);
        metrics.state.emit(State::Starting as i64);
        let task = launch(def, &ep);
        let started = clock_now();
        metrics.state.emit(State::Running as i64);
        live.push(Live {
            id: *id,
            task: Some(task),
            started,
            metrics,
            history: RestartHistory { consecutive_failures: 0, recent: Vec::new() },
        });
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
        let uptime = now.saturating_sub(slot.started);
        let m = slot.metrics;
        let def = def_for(&defs, id);

        // This incarnation is gone — record how long it lived and note the exit as
        // a point-event span (`let _` closes it immediately: SpanStart + SpanEnd).
        m.uptime.emit(uptime as i64);
        let _ = tracer().span(&format!("svc.{}.exited.{status}", def.name));

        let outcome = if status == 0 { ExitOutcome::Clean } else { ExitOutcome::Failed(status) };
        match restart_decision(def.policy, outcome, &slot.history, def.limits, now) {
            RestartAction::Stop => {
                m.state.emit(State::Stopped as i64);
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

                // Back off (state Backoff, the wait visible as backoff_ticks), then
                // relaunch and return to Running.
                m.state.emit(State::Backoff as i64);
                m.backoff.emit(after as i64);
                wait_until(now + after);
                let task = launch(def, &ep);
                if let Some(slot) = live.iter_mut().find(|l| l.id == id) {
                    slot.task = Some(task);
                    slot.started = clock_now();
                }
                m.state.emit(State::Running as i64);
                m.restarts.emit(restarts);
            }
        }
    }
}
