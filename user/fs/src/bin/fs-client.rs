//! v0.10 FS client (`workload=fs`).
//!
//! 1. **Connect:** `call` on the bare endpoint cap (`badge == 0` = "attach");
//!    the server mints `pack(root, READ)` and returns it.
//! 2. **Stat:** `call` `Request::Stat` on the root File cap; decode the reply.
//!    Emit a sentinel **only** when the root reads back as an empty `Dir` — so
//!    the itest sees the full request/response round-trip carried the right
//!    data across the process boundary.

#![no_std]
#![no_main]

use fs_core::NodeKind;
use fs_proto::{Op, Request, Response};
use snitchos_user::{endpoint, entry, telemetry, Endpoint};

#[entry]
fn main() {
    let Ok((_resp, Some(cap))) = endpoint().call([0, 0, 0, 0]) else {
        return;
    };
    let root = Endpoint::from_raw_handle(cap);

    let Ok((words, _)) = root.call(Request::Stat.encode()) else {
        return;
    };
    if let Ok(Response::Stat(s)) = Response::decode(Op::Stat, words)
        && s.kind == NodeKind::Dir
        && s.size == 0
    {
        let _ = telemetry().emit(0x57A7);
    }
}
