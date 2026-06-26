//! RPC demo client (`workload=ipc-rpc`): makes two traced `call`s so the
//! `reply_recv` server serves more than one request — exercising reply-cap
//! **slot reuse** (the second reply cap reuses the first's freed `CapTable`
//! slot). Each call runs inside its own `rpc.call` span, so the server's
//! `rpc.handle` nests under it — two clean nested round-trips in the trace.
//! Each re-emits its response so the itest can confirm both completed.
//! crt0 / syscalls from `snitchos-user`.

#![no_std]
#![no_main]

use snitchos_user::{Metric, endpoint, entry, register_counter, tracer};

#[entry]
fn main() {
    // Register our marker metric once; both round-trips emit through its handle.
    let marker = register_counter("snitchos.rpc_client.marker");
    rpc(marker, 21); // 21 -> 42
    rpc(marker, 50); // 50 -> 100 — proves the reply_recv loop served a second request
}

/// One traced round-trip: open `rpc.call`, `call` inside it (so the span stays
/// open across the round-trip and the server's `rpc.handle` nests under it),
/// then re-emit the response.
fn rpc(marker: Metric, req: u64) {
    let _span = tracer().span("rpc.call");
    // The server replies with no cap, so ignore the transferred-handle slot.
    if let Ok((resp, _cap)) = endpoint().call([req, 0, 0, 0]) {
        marker.emit(resp[0] as i64);
    }
}
