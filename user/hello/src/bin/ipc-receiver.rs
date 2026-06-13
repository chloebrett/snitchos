//! IPC demo receiver (`workload=ipc`): receives one inline message over its
//! `Endpoint` capability (granted `RECV` at bootstrap), then re-emits the
//! first word through its `TelemetrySink` so the itest can observe that the
//! payload crossed the process boundary. The receive blocks until the sender
//! rendezvouses. crt0 / panic / syscalls come from the `snitchos-user` runtime.

#![no_std]
#![no_main]

use snitchos_user::{endpoint, entry, telemetry, tracer};

#[entry]
fn main() {
    // Blocks until the sender rendezvouses. The kernel seeds the sender's span
    // as our incoming trace context, so the handling span we open next roots
    // under it — proving the trace crossed the process boundary.
    if let Ok(words) = endpoint().receive() {
        let _span = tracer().span("ipc.recv");
        let _ = telemetry().emit(words[0] as i64);
    }
}
