//! v0.10 FS client (`workload=fs`) — **step 2a: connect.**
//!
//! `call`s the FS on its bare endpoint cap (`badge == 0` = "attach"); the
//! server mints `pack(root, READ)` and returns it. Wrapping the returned handle
//! and emitting a marker proves the transferred cap arrived; the kernel also
//! snitches the transfer (`CapEvent::Transferred`), which the itest asserts on.
//! Issuing `Stat` over the root cap lands in step 2b.

#![no_std]
#![no_main]

use snitchos_user::{endpoint, entry, telemetry, Endpoint};

#[entry]
fn main() {
    if let Ok((_resp, Some(cap))) = endpoint().call([0, 0, 0, 0]) {
        let _root = Endpoint::from_raw_handle(cap);
        let _ = telemetry().emit(1);
    }
}
