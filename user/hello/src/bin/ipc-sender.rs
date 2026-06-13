//! IPC demo sender (`workload=ipc`): sends one inline message over its
//! `Endpoint` capability (granted `SEND` at bootstrap), then exits. The send
//! is a rendezvous — it blocks until the receiver arrives. The sentinel value
//! is what the receiver re-emits, so the itest can assert the payload crossed
//! the process boundary. crt0 / panic / syscalls come from the `snitchos-user`
//! runtime.

#![no_std]
#![no_main]

use snitchos_user::{endpoint, entry, tracer};

/// The payload the receiver re-emits — the itest asserts this value crosses.
const SENTINEL: u64 = 42;

#[entry]
fn main() {
    // Open a span and send *inside* it: the kernel captures this span as the
    // message's trace context, so the receiver's handling span roots under it
    // — the trace following the message across the process boundary.
    let _span = tracer().span("ipc.send");
    let _ = endpoint().send([SENTINEL, 0, 0, 0]);
}
