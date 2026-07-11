//! `workload=view-demo`: connects to the seeded FS, looks up `bin/spawnee`
//! with READ-only rights, then spawns the viewer with that cap delegated.
//! Exercises the full powerbox hand-off: mint an attenuated cap, delegate
//! it across the Spawn boundary, let the child read and print the file.

#![no_std]
#![no_main]

use fs_proto::{FileRights, Op, Request, Response, UserBuf};
use snitchos_user::{Endpoint, bootstrap, entry, notify_create, revoke, spawn, wait};

/// SPAWNABLE index for the viewer binary (see `kernel/src/trap/user.rs`).
const VIEWER_ID: usize = 6;

#[entry(needs = [("fs", ENDPOINT, SEND)])]
fn main() {
    let Some(fs) = bootstrap().get::<Endpoint>("fs") else {
        return;
    };

    // Attach to FS root.
    let Ok((_r, Some(root_cap))) = fs.call([0, 0, 0, 0]) else {
        return;
    };
    let root = Endpoint::from_raw_handle(root_cap);

    // Navigate to bin/.
    let bin = b"bin";
    let lookup_bin = Request::Lookup {
        name: UserBuf { ptr: bin.as_ptr() as u64, len: bin.len() as u64 },
        rights: FileRights::READ,
    };
    let Ok((_l, Some(bin_cap))) = root.call(lookup_bin.encode()) else {
        return;
    };
    let bin_dir = Endpoint::from_raw_handle(bin_cap);

    // Look up spawnee with READ-only rights — the attenuated cap to delegate.
    let name = b"spawnee";
    let lookup_file = Request::Lookup {
        name: UserBuf { ptr: name.as_ptr() as u64, len: name.len() as u64 },
        rights: FileRights::READ,
    };
    let Ok((_f, Some(file_cap))) = bin_dir.call(lookup_file.encode()) else {
        return;
    };

    // Verify the file has bytes before handing it off (stat it).
    let file = Endpoint::from_raw_handle(file_cap);
    let Ok((stat_words, _)) = file.call(Request::Stat.encode()) else {
        return;
    };
    if let Ok(Response::Stat(s)) = Response::decode(Op::Stat, stat_words)
        && s.size == 0
    {
        return;
    }

    // Two-phase readiness handshake with the viewer (replaces the old yield-count
    // timing bet): `done` = the viewer signals it finished reading; `proceed` = we
    // release it to exit after revoking. Both are ambient to create.
    let done = notify_create();
    let proceed = notify_create();

    // Spawn the viewer with the file cap + both notification caps, in slot order
    // (file, done, proceed) — the viewer resolves each by role via bootstrap().get.
    let handles = [file_cap as u32, done.raw_handle() as u32, proceed.raw_handle() as u32];
    if let Some(child) = spawn(VIEWER_ID, &handles) {
        // Wait until the viewer has finished reading, THEN reclaim — so Revoked lands
        // after bytes_read on the wire (grant → use → reclaim), deterministically.
        let _ = done.wait();
        let _ = revoke(file_cap);
        // Release the viewer (it's blocked in WaitNotify, alive, so the revoke above
        // fired a real CapEvent::Revoked), then reap it.
        let _ = proceed.signal(1);
        let _ = wait(child);
    }
}
