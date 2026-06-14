#![no_std]
#![no_main]

use snitchos_user::{endpoint, entry, reply_with_cap, rights};

// v0.9c cap-transfer-in-reply server. Holds `RECV | MINT` on the shared
// endpoint. For each request, it mints a badged `SEND` cap (badge = the
// caller's requested word 0) and **hands it back in the reply** — so the client
// ends up holding an unforgeable, badged path to this server. The whole point:
// a server can return capabilities, not just data.
#[entry]
fn main() {
    loop {
        let Ok((req, Some(reply_handle))) = endpoint().receive_with_reply() else {
            continue;
        };
        let Ok(cap) = endpoint().mint_badged(req[0], rights::SEND) else {
            continue;
        };
        let _ = reply_with_cap(reply_handle, [0, 0, 0, 0], cap);
    }
}
