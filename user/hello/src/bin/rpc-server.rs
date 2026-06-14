//! RPC demo server (`workload=ipc-rpc`): the canonical `reply_recv` loop —
//! reply the previous client and block for the next request in one syscall.
//! Each request opens an `rpc.handle` span (nested under the client's `rpc.call`
//! via the kernel-propagated trace context) and computes `req * 2`. crt0 /
//! syscalls from `snitchos-user`.

#![no_std]
#![no_main]

use snitchos_user::{endpoint, entry, tracer};

#[entry]
fn main() {
    // `prev` carries (reply handle, response) for the request handled last
    // iteration; `None` on the first. `reply_recv` answers it (if any) then
    // blocks for the next request.
    let mut prev: Option<(usize, [u64; 4])> = None;
    loop {
        let r = match endpoint().reply_recv(prev) {
            Ok(next) => next,
            Err(_) => return,
        };
        let _span = tracer().span("rpc.handle");
        // Reply only to a `call` (reply handle present); a one-way `send` has none.
        prev = r.reply.map(|h| (h, [r.msg[0] * 2, 0, 0, 0]));
    }
}
