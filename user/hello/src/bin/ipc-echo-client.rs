//! `ipc-echo-client` — the persistent client in the supervision FU2 survival demo.
//! It holds **one** minted `SEND` cap on the supervisor's durable endpoint and
//! sends a short series of one-way messages. Each `send` is a rendezvous: it
//! completes only when a live server incarnation receives it. Because the server
//! crashes after every request and is restarted, consecutive sends land on
//! *different* incarnations — yet the client never re-acquires its cap. A second
//! completed send (after a restart) proves the minted cap survived, because it
//! names the durable endpoint object, not any one server process.

#![no_std]
#![no_main]

use snitchos_user::{Endpoint, delegated_handle, entry, exit, register_counter};

/// How many requests to send. Kept small so the run is quick; each one forces a
/// server incarnation to serve-and-die, so this many sends ⇒ this many
/// incarnations (one restart between each).
const REQUESTS: u64 = 3;

#[entry]
fn main() {
    let ep = Endpoint::from_raw_handle(delegated_handle(0));
    // Register the counter once and reuse the handle (the metric table is bounded
    // and does not dedup — re-registering per send would leak slots).
    let sent = register_counter("snitchos.ipcclient.sent");

    let mut i = 0u64;
    while i < REQUESTS {
        // Rendezvous send — blocks until *some* live server incarnation receives it.
        if ep.send([i, 0, 0, 0]).is_err() {
            break;
        }
        i += 1;
        sent.emit(i as i64); // 1, 2, 3 — each a completed round-trip
    }

    exit();
}
