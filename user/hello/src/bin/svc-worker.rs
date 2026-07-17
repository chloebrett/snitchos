//! `svc-worker` — a cooperative supervised service (v2a graceful shutdown).
//!
//! Spawned by the `supervised-shutdown` supervisor holding one delegated shutdown
//! [`Notification`] (at the first delegated handle). It proves it came up
//! (`snitchos.svcworker.up`), then parks in `WaitNotify` — when the supervisor
//! `Signal`s the shutdown notification, it wakes and `exit(0)`s **cleanly**. That
//! clean exit is what distinguishes a cooperative service (stopped by a signal it
//! opted into) from a forced one the supervisor must `Kill`.
//!
//! Notifications coalesce, so a signal that races ahead of the `wait` isn't lost —
//! the `wait` returns the pending bits immediately.

#![no_std]
#![no_main]

use snitchos_user::{delegated_handle, entry, exit_with, register_counter, Notification};

#[entry]
fn main() {
    // Liveness: prove this incarnation reached its run loop.
    register_counter("snitchos.svcworker.up").emit(1);

    // The supervisor delegated a shutdown notification at the first delegated handle.
    // Block on it; on the supervisor's `Signal`, exit cleanly (status 0).
    let shutdown = Notification::from_raw_handle(delegated_handle(0));
    let _ = shutdown.wait();
    exit_with(0);
}
