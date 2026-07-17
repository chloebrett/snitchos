//! `hung-service` (SPAWNABLE) — the wedged service for the hung-detection itest (v2b).
//!
//! `Spawn`ed by `hung-supervisor` holding a delegated liveness [`Notification`]
//! (SIGNAL end, at the first delegated handle). It **beats** a few times — signalling
//! the notification, each a proof of progress the supervisor observes via
//! `wait_timeout` — then **wedges**: a tight `loop {}` that keeps the process *alive*
//! but stops beating. The supervisor's next timed wait then times out, which is how it
//! detects a live-but-stuck service and force-`Kill`s it. It never exits on its own.

#![no_std]
#![no_main]

use snitchos_user::{delegated_handle, entry, Notification};

#[entry]
fn main() {
    let liveness = Notification::from_raw_handle(delegated_handle(0));

    // A few liveness beats, spaced by a brief spin so they're distinct signals (not
    // coalesced into one) and the supervisor sees progress before the wedge.
    let mut beat = 0;
    while beat < 3 {
        let _ = liveness.signal(0b1);
        let mut spin = 0u64;
        while spin < 200_000 {
            core::hint::spin_loop();
            spin += 1;
        }
        beat += 1;
    }

    // Wedge: alive, but no more beats. The supervisor's `wait_timeout` now times out.
    loop {
        core::hint::spin_loop();
    }
}
