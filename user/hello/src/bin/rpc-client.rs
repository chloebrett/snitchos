//! RPC demo client (`workload=ipc-rpc`): `call`s the server with a request and
//! blocks for the reply, then re-emits the response's first word so the itest
//! can confirm the round-trip. The `call` runs inside an `rpc.call` span that
//! stays open across the whole round-trip — so the server's handling span nests
//! under it (the RPC flame-graph shape). crt0 / syscalls from `snitchos-user`.

#![no_std]
#![no_main]

use snitchos_user::{endpoint, entry, telemetry, tracer};

/// The request value. The server replies `REQUEST * 2`, so the emitted
/// response (42) proves it was computed server-side, not echoed.
const REQUEST: u64 = 21;

#[entry]
fn main() {
    let _span = tracer().span("rpc.call");
    if let Ok(resp) = endpoint().call([REQUEST, 0, 0, 0]) {
        let _ = telemetry().emit(resp[0] as i64);
    }
}
