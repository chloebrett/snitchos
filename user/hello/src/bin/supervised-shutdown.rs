//! `workload=supervised-shutdown` — the graceful reverse-dependency shutdown
//! supervisor (supervision v2a, increment 4).
//!
//! Where `supervised` demonstrates bringing a service **back** (crash-restart), this
//! demonstrates taking a tree **down** deliberately, in the exact mirror of how it
//! came up. It brings a small dependency chain up in `startup_order`, then walks
//! `teardown_order` (the reverse) stopping one service at a time:
//!
//! - **cooperative** services opted into a shutdown [`Notification`] the supervisor
//!   delegated at spawn — it `Signal`s that, and the service `exit(0)`s cleanly;
//! - the **forced** service (a `spinner` that never cooperates) is force-terminated
//!   via `kill`, spending the `Object::Process` lifecycle cap the kernel minted at
//!   `Spawn` (a `CapEvent::Revoked` on the wire).
//!
//! Each stop emits `snitchos.svc.<name>.stopped` **after** the service is reaped, so
//! the emission order *is* the reverse-dependency proof: the tree comes down in the
//! exact reverse of how it went up.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::vec::Vec;

use snitchos_user::{
    entry, exit_with, kill, notify_create, register_counter, spawn_supervised, tracer, wait_any,
    Child, Notification,
};
use supervision::{startup_order, teardown_order, ServiceId, ServiceSpec};

/// The single shutdown bit the supervisor asserts on a cooperative service's
/// notification. Its meaning is a convention between supervisor and service.
const SHUTDOWN_BIT: u64 = 0b1;

/// How a service is stopped at teardown.
#[derive(Clone, Copy)]
enum Stop {
    /// Opted into a shutdown notification: `Signal` it and it exits cleanly.
    Cooperative,
    /// Never cooperates (a `spinner`): force-terminate via its `Object::Process` cap.
    Forced,
}

/// A supervised service: the pure `ServiceSpec` (id + deps, for ordering) plus the
/// runtime knobs the engine needs to launch and stop it.
struct Svc {
    spec: ServiceSpec,
    name: &'static str,
    /// `SPAWNABLE` registry id.
    program: usize,
    stop: Stop,
}

/// Ids are indices into the table, so `deps` reads naturally. The chain is
/// `alpha → beta → gamma` (gamma depends on beta depends on alpha), so startup is
/// `[alpha, beta, gamma]` and teardown the reverse `[gamma, beta, alpha]`.
const ALPHA: ServiceId = ServiceId(0);
const BETA: ServiceId = ServiceId(1);
const GAMMA: ServiceId = ServiceId(2);

/// `SPAWNABLE` ids: the cooperative worker, and the forced spinner (never exits).
const SVC_WORKER: usize = 10;
const SPINNER: usize = 3;

fn services() -> [Svc; 3] {
    [
        Svc { spec: ServiceSpec { id: ALPHA, deps: &[] }, name: "alpha", program: SVC_WORKER, stop: Stop::Cooperative },
        Svc { spec: ServiceSpec { id: BETA, deps: &[ALPHA] }, name: "beta", program: SVC_WORKER, stop: Stop::Cooperative },
        Svc { spec: ServiceSpec { id: GAMMA, deps: &[BETA] }, name: "gamma", program: SPINNER, stop: Stop::Forced },
    ]
}

/// Live bookkeeping for one running service.
struct Live {
    id: ServiceId,
    /// The child + its lifecycle (`Object::Process`) cap handle, for `kill`.
    child: Child,
    /// The shutdown notification we delegated — `Some` for a cooperative service.
    notify: Option<Notification>,
}

fn def_for(defs: &[Svc], id: ServiceId) -> &Svc {
    defs.iter().find(|d| d.spec.id == id).expect("id is from the same table")
}

/// A fatal supervision error (bad config, or a service we couldn't even start). At
/// the root there is no parent to escalate to, so snitch it and halt.
fn halt(reason: &str) -> ! {
    let _fatal = tracer().span(&format!("supervised_shutdown.halt.{reason}"));
    register_counter("snitchos.supervised_shutdown.halted").emit(1);
    exit_with(1);
}

#[entry]
fn main() {
    let defs = services();
    let specs: Vec<ServiceSpec> = defs.iter().map(|d| d.spec).collect();

    // Bring services up in dependency order. Cooperative services are handed a fresh
    // shutdown notification (delegated at their first handle); the forced one gets
    // none — it's stopped by `kill`, not a signal.
    let up = match startup_order(&specs) {
        Ok(order) => order,
        Err(_) => halt("dependency-cycle"),
    };
    let mut live: Vec<Live> = Vec::with_capacity(up.len());
    for id in &up {
        let def = def_for(&defs, *id);
        let (handles, notify): (Vec<u32>, Option<Notification>) = match def.stop {
            Stop::Cooperative => {
                let n = notify_create();
                (alloc::vec![n.raw_handle() as u32], Some(n))
            }
            Stop::Forced => (Vec::new(), None),
        };
        let child = match spawn_supervised(def.program, &handles) {
            Some(child) => child,
            None => halt("spawn-failed"),
        };
        register_counter(&format!("snitchos.svc.{}.started", def.name)).emit(1);
        live.push(Live { id: *id, child, notify });
    }

    // Tear down in the exact reverse of startup, one service at a time — stop it,
    // reap it, emit `stopped`, then move on — so the emissions land in strict
    // reverse-dependency order.
    let down = match teardown_order(&specs) {
        Ok(order) => order,
        Err(_) => halt("dependency-cycle"),
    };
    for id in &down {
        let def = def_for(&defs, *id);
        let svc = live.iter().find(|l| l.id == *id).expect("live entry for an ordered id");
        match def.stop {
            Stop::Cooperative => {
                // Signal the shutdown the service opted into; it wakes and exits(0).
                if let Some(notify) = svc.notify {
                    let _ = notify.signal(SHUTDOWN_BIT);
                }
            }
            Stop::Forced => {
                // No cooperative path — force-terminate via the lifecycle cap.
                let _ = kill(svc.child.kill);
            }
        }
        // Reap exactly this child before the next stop. We stop strictly serially, so
        // `wait_any` returns this one; the loop guards against any stray exit.
        let target = svc.child.task;
        loop {
            let (_status, child) = wait_any();
            if child == target {
                break;
            }
        }
        let _span = tracer().span(&format!("svc.{}.stopped", def.name));
        register_counter(&format!("snitchos.svc.{}.stopped", def.name)).emit(1);
    }

    register_counter("snitchos.supervised_shutdown.complete").emit(1);
    exit_with(0);
}
