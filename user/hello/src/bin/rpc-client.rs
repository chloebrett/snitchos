//! RPC demo client (`workload=ipc-rpc`): makes two `call`s so the `reply_recv`
//! server serves more than one request — exercising reply-cap **slot reuse**
//! (the second reply cap reuses the first's freed `CapTable` slot). The first
//! call runs inside an `rpc.call` span (the nested-trace headline); the second
//! is the reuse stressor. Each re-emits its response so the itest can confirm
//! both round-trips. crt0 / syscalls from `snitchos-user`.

#![no_std]
#![no_main]

use snitchos_user::{endpoint, entry, telemetry, tracer};

#[entry]
fn main() {
    // First round-trip, traced: 21 -> 42. The `rpc.call` span stays open across
    // the call so the server's `rpc.handle` nests under it.
    {
        let _span = tracer().span("rpc.call");
        if let Ok(resp) = endpoint().call([21, 0, 0, 0]) {
            let _ = telemetry().emit(resp[0] as i64); // 42
        }
    }
    // Second round-trip: 50 -> 100. Proves the `reply_recv` loop served a second
    // request and reused the reply-cap slot.
    if let Ok(resp) = endpoint().call([50, 0, 0, 0]) {
        let _ = telemetry().emit(resp[0] as i64); // 100
    }
}
