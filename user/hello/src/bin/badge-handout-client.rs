#![no_std]
#![no_main]

use snitchos_user::{endpoint, entry, Endpoint};

// v0.9c headline client. `call`s the handout server to receive a badged cap
// (its kernel-minted identity), then **sends on that badged cap** — so the
// server's receive surfaces this client's badge and can tell it apart from its
// peers. Spawned more than once; each instance gets a distinct badge.
#[entry]
fn main() {
    if let Ok((_resp, Some(cap))) = endpoint().call([0, 0, 0, 0]) {
        let _ = Endpoint::from_raw_handle(cap).send([0, 0, 0, 0]);
    }
}
