//! `workload=supervised-ipc` — the supervision cap-survival demo (FU2).
//!
//! The supervisor owns a durable endpoint (`svc-ep`) and, as a manifest satisfier,
//! grants from it: a minted `SEND` to a **persistent client** and a minted `RECV`
//! to a **crashing server**. The server serves one request then exits; the
//! supervisor re-satisfies + respawns it. The client holds its **one** minted
//! `SEND` cap the whole time and keeps sending — each send rendezvous with whichever
//! server incarnation is currently alive. That the client's cap keeps working across
//! server restarts is the point: a minted cap names the durable *object*, not the
//! process, so it survives the process dying. This is the D3 payoff — "clients
//! survive the restart transparently" — proven by a real IPC round-trip.
//!
//! Unlike `workload=supervised` (which drives the full policy: backoff, intensity,
//! escalate), this is a focused survival demo: the server is simply respawned on
//! every exit. The restart policy itself is proven over there.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;

use hitch::{CapView, Grant, Slot, satisfy};
use snitchos_user::{
    Endpoint, delegated_handle, endpoint_create, entry, exit, object_kind, register_counter,
    rights, spawn,
};

/// SPAWNABLE registry ids (see `kernel/src/trap/user.rs`).
const IPC_ECHO_SERVER: usize = 8;
const IPC_ECHO_CLIENT: usize = 9;

/// Satisfy a child that declares it needs `need_rights` on our endpoint, minting
/// the attenuated cap from our own `RECV | MINT`, and spawn it. Returns the new
/// task id, or `None` if satisfy / mint / spawn refused.
fn satisfy_and_spawn(program: usize, need_rights: u32, ep: &Endpoint) -> Option<u32> {
    let have = [CapView {
        object: object_kind::ENDPOINT as u8,
        rights: rights::RECV | rights::SEND | rights::MINT,
        handle: ep.raw_handle() as u32,
    }];
    let needs = [Slot { name: "svc-ep".into(), object: object_kind::ENDPOINT as u8, rights: need_rights }];

    let plan = satisfy(&needs, &have).ok()?;
    let mut handles: Vec<u32> = Vec::with_capacity(plan.len());
    for grant in &plan {
        let handle = match grant {
            Grant::Use { handle } => *handle,
            Grant::Mint { from, rights } => {
                Endpoint::from_raw_handle(*from as usize).mint_badged(0, *rights).ok()? as u32
            }
        };
        handles.push(handle);
    }
    spawn(program, &handles)
}

#[entry]
fn main() {
    let ep = endpoint_create("svc-ep");

    // The persistent client (minted SEND) and the crashing server (minted RECV).
    let client = satisfy_and_spawn(IPC_ECHO_CLIENT, rights::SEND, &ep);
    let mut server = satisfy_and_spawn(IPC_ECHO_SERVER, rights::RECV, &ep);

    // Register the restart counter once; reuse the handle (bounded metric table).
    let restarts_metric = register_counter("snitchos.svc.server.restarts_total");
    let mut restarts: i64 = 0;

    // Supervise: respawn the server on every exit (re-satisfying its RECV against
    // the same durable endpoint), until the client finishes and is reaped.
    loop {
        let (status, child) = snitchos_user::wait_any();

        if client == Some(child) {
            register_counter("snitchos.supervisedipc.client_reaped").emit(i64::from(status));
            exit();
        }

        if server == Some(child) {
            restarts += 1;
            restarts_metric.emit(restarts);
            server = satisfy_and_spawn(IPC_ECHO_SERVER, rights::RECV, &ep);
        }
    }
}
