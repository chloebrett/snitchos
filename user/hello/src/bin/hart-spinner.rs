//! `hart-spinner` (SPAWNABLE) — the victim for the cross-hart Kill itest (v2b).
//!
//! `SpawnOn`'d to hart 0 by the `xhart-kill` supervisor (which runs on hart 1). It
//! opens one span to prove it reached U-mode on hart 0 (the `SpanStart` carries
//! `hart_id == 0`), then **tight-loops** — no yield, no syscall — so it stays
//! *running* on hart 0. That keeps it in `kill_task`'s `running_remote` case, which is
//! exactly the cross-hart path under test: the kill flags it + IPIs hart 0, and it
//! self-terminates at hart 0's return-to-user checkpoint. It never exits on its own.

#![no_std]
#![no_main]

use snitchos_user::{entry, tracer};

#[entry]
fn main() {
    // Liveness: proof this ran on hart 0 (the SpanStart is emitted from hart 0).
    let _ = tracer().span("hart_spinner.up");
    // Tight spin — stay Running so the cross-hart kill hits `running_remote`. The timer
    // quantum preempts us periodically; we're immediately rescheduled.
    loop {
        core::hint::spin_loop();
    }
}
