//! `workload=xhart-kill` — the cross-hart Kill supervisor (v2b step 4).
//!
//! Runs on hart 1 (userspace's normal hart). It `SpawnOn`s a `hart-spinner` victim
//! onto **hart 0**, lets it run there, then `kill`s it — the cross-hart case: the
//! target is *running on another core*, so `kill_task` can't reap it out-of-band.
//! Instead it flags the victim and IPIs hart 0, which self-terminates it at its
//! return-to-user checkpoint. The supervisor reaps it via `WaitAny` and reports.

#![no_std]
#![no_main]

use snitchos_user::{
    entry, exit_with, kill, register_counter, spawn_supervised_on, wait_any, yield_now,
};

/// SPAWNABLE id of the `hart-spinner` victim.
const HART_SPINNER: usize = 11;

#[entry]
fn main() {
    // Place the victim on hart 0 (we run on hart 1) — the cross-hart setup. We get its
    // task id *and* its lifecycle (`Process`/`KILL`) cap handle back.
    let Some(victim) = spawn_supervised_on(HART_SPINNER, &[], 0) else {
        register_counter("snitchos.xhart.spawn_failed").emit(1);
        exit_with(1);
    };

    // Let hart 0 schedule + run the victim, so the kill lands while it is *running*
    // there (exercising the `running_remote` path). Yielding on hart 1 passes
    // wall-clock; hart 0 runs the victim in parallel.
    let mut spins = 0;
    while spins < 5000 {
        yield_now();
        spins += 1;
    }

    // Cross-hart kill: flags the victim + IPIs hart 0; it self-terminates at hart 0's
    // return-to-user checkpoint. The cap is spent (a `CapEvent::Revoked` on the wire).
    let _ = kill(victim.kill);

    // Reap it once its hart honours the flag.
    let (_status, _child) = wait_any();
    register_counter("snitchos.xhart.reaped").emit(1);
    exit_with(0);
}
