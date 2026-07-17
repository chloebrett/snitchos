//! `workload=hung-detect` — the hung-detection supervisor (v2b).
//!
//! Detects a service that is *alive but wedged* — the case neither crash-restart nor
//! graceful shutdown covers, because a wedged service never `Exit`s (so `WaitAny`
//! blocks forever) and never cooperates (so a shutdown `Signal` is ignored). The only
//! way to notice "stuck" is a **deadline**.
//!
//! It owns a liveness [`Notification`], delegates the SIGNAL end to a `hung-service`
//! child (which beats a few times then wedges), and loops `wait_timeout` on the WAIT
//! end: each beat within the budget is healthy (`beats_seen`); the first **timeout**
//! means no progress ⇒ wedged ⇒ `hung_detected` ⇒ `kill` the service + reap it.

#![no_std]
#![no_main]

use snitchos_user::{
    clock_freq, clock_now, entry, exit_with, kill, notify_create, register_counter,
    spawn_supervised, wait_any, Notification,
};

/// SPAWNABLE id of the `hung-service` victim.
const HUNG_SERVICE: usize = 12;

#[entry]
fn main() {
    // Liveness notification: keep the WAIT end, delegate the SIGNAL end to the child.
    let liveness = notify_create();
    let Some(child) = spawn_supervised(HUNG_SERVICE, &[liveness.raw_handle() as u32]) else {
        register_counter("snitchos.hung.spawn_failed").emit(1);
        exit_with(1);
    };

    let beats = register_counter("snitchos.hung.beats_seen");
    // Liveness budget: no beat within this window ⇒ the service is wedged. ~250 ms
    // (well above the 50 ms timer granularity, well below any real beat interval).
    let budget = clock_freq() / 4;

    loop {
        let deadline = clock_now() + budget;
        match liveness.wait_timeout(deadline) {
            // A beat arrived in time — the service is making progress.
            Ok(Some(_bits)) => beats.emit(1),
            // No beat within the budget — the service is hung. Force-stop + reap it.
            Ok(None) => {
                register_counter("snitchos.hung.detected").emit(1);
                let _ = kill(child.kill);
                let _ = wait_any();
                register_counter("snitchos.hung.reaped").emit(1);
                exit_with(0);
            }
            // Cap refused (shouldn't happen — we own the notification).
            Err(_) => exit_with(2),
        }
    }
}
