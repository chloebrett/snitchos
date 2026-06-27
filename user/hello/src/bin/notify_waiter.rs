//! `workload=notify-smoke` parent — the v0.12 notification demo waiter.
//!
//! Creates a notification, `Spawn`s the `notify-signaller` child (spawnable id 2)
//! delegating the notification cap, then `WaitNotify`s on it. The child `Signal`s
//! a known bit mask; this parent wakes and emits the bits it received as
//! `snitchos.notify.bits`. Proves the async kernel→user wake crosses the task
//! boundary — a `NotifySignal`→`NotifyWait` edge on the wire.

#![no_std]
#![no_main]

use snitchos_user::{entry, exit, notify_create, register_counter, spawn};

#[entry]
fn main() {
    let notif = notify_create();

    // Hand the child exactly the notification cap. Spawnable id 2 = notify-signaller.
    let Some(_child) = spawn(2, &[notif.raw_handle() as u32]) else {
        register_counter("snitchos.notify.refused").emit(1);
        exit();
    };

    // Block until the child signals (or take the coalesced bits if it already
    // ran). The bits we receive are the mask the signaller chose.
    match notif.wait() {
        Ok(bits) => register_counter("snitchos.notify.bits").emit(bits as i64),
        Err(_) => register_counter("snitchos.notify.refused").emit(1),
    }
    exit();
}
