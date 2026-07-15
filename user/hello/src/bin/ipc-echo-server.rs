//! `ipc-echo-server` — a crashing supervised IPC server (supervision FU2). It
//! holds a re-granted `RECV` cap on the supervisor's durable endpoint, serves
//! **one** request, then exits non-zero so the supervisor restarts it. Each
//! incarnation is a fresh process with a freshly-delegated `RECV` cap; the
//! *client's* minted `SEND` cap names the same durable endpoint object, so it
//! keeps working across these restarts — that's the survival the FU2 itest proves.

#![no_std]
#![no_main]

use snitchos_user::{Endpoint, delegated_handle, entry, exit_with, register_counter};

#[entry]
fn main() {
    // Our delegated endpoint (minted RECV) lands at the first delegated handle.
    let ep = Endpoint::from_raw_handle(delegated_handle(0));

    // Rendezvous: block until the client sends, then note the payload we served.
    match ep.receive() {
        Ok(msg) => register_counter("snitchos.ipcserver.served").emit(msg[0] as i64),
        Err(_) => exit_with(2),
    }

    // Crash after serving one request — forces the supervisor to restart us, so the
    // client's next send lands on a *fresh* incarnation over the same endpoint.
    exit_with(17);
}
