//! RPC demo server (`workload=ipc-rpc`): `receive`s a request together with a
//! one-shot reply cap, opens an `rpc.handle` span (nested under the client's
//! `rpc.call` via the kernel-propagated trace context), computes `req * 2`, and
//! `reply`s — waking the blocked client. crt0 / syscalls from `snitchos-user`.

#![no_std]
#![no_main]

use snitchos_user::{endpoint, entry, reply, tracer};

#[entry]
fn main() {
    // Receive the request and the reply handle (a `call`, so it's `Some`).
    if let Ok((req, Some(reply_handle))) = endpoint().receive_with_reply() {
        let _span = tracer().span("rpc.handle");
        let _ = reply(reply_handle, [req[0] * 2, 0, 0, 0]);
    }
}
