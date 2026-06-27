//! The `notify-signaller` child (spawnable id 2) — v0.12 notification demo.
//!
//! Spawned by `notify-waiter` holding one delegated `Notification` cap (at the
//! first delegated handle). Signals it with a known bit mask — waking the parent
//! parked in `WaitNotify` — then exits. If the cap hadn't been delegated, the
//! `Signal` would be refused and the parent would block forever.

#![no_std]
#![no_main]

use snitchos_user::{delegated_handle, entry, exit, Notification};

#[entry]
fn main() {
    // The parent delegated its notification cap; it lands at the first delegated
    // handle. Signal the mask the parent asserts it received (0b101 = 5).
    let notif = Notification::from_raw_handle(delegated_handle(0));
    let _ = notif.signal(0b101);
    exit();
}
