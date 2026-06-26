#![no_std]
#![no_main]

use snitchos_user::{endpoint, entry, register_counter, reply_with_cap, rights};

// v0.9c headline server. Holds `RECV | MINT` on one endpoint and serves many
// clients over it:
//   - a `call` (reply handle present) asks for a cap → mint a **distinct**
//     badged `SEND` cap (an incrementing per-client identity) and hand it back.
//   - a one-way `send` (no reply handle) arrives on a badged cap → the kernel
//     delivered the sender's badge in `r.badge`; re-emit it as telemetry so the
//     wire shows the server told its clients apart **by capability, not by who
//     they are** — one endpoint, one receive loop, demuxed by badge.
#[entry]
fn main() {
    let mut next: u64 = 0xBEE1;
    let received_badge = register_counter("snitchos.badge_handout.marker");
    loop {
        let Ok(r) = endpoint().receive_with_reply() else {
            continue;
        };
        match r.reply {
            Some(reply_handle) => {
                if let Ok(cap) = endpoint().mint_badged(next, rights::SEND) {
                    let _ = reply_with_cap(reply_handle, [0, 0, 0, 0], cap);
                    next += 1;
                }
            }
            None => {
                received_badge.emit(r.badge as i64);
            }
        }
    }
}
