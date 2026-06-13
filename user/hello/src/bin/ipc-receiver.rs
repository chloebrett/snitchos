//! IPC demo receiver (`workload=ipc`): receives one inline message over its
//! `Endpoint` capability (granted `RECV` at bootstrap), then re-emits the
//! first word through its `TelemetrySink` so the itest can observe that the
//! payload crossed the process boundary. The receive blocks until the sender
//! rendezvouses. crt0 / panic / syscalls come from the `snitchos-user` runtime.

#![no_std]
#![no_main]

use snitchos_user::{endpoint, entry, telemetry};

#[entry]
fn main() {
    // Blocks until the sender rendezvouses, then surfaces the payload's first
    // word as telemetry — the wire signal the itest asserts on.
    if let Ok(words) = endpoint().receive() {
        let _ = telemetry().emit(words[0] as i64);
    }
}
