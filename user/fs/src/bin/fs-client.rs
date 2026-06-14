//! v0.10 FS client (`workload=fs`) — exercises the FS over capabilities:
//!
//! 1. **Connect:** `call` on the bare endpoint cap (`badge == 0` = "attach");
//!    the server mints `pack(root, READ)` and returns it.
//! 2. **Stat root:** confirm the root reads back as an empty `Dir` → emit
//!    `0x57A7` (`fs-stat-root`).
//! 3. **Create:** `call` `Request::Create` on the root cap — the filename rides
//!    as a `UserBuf` the kernel copies across (option D) — and receive the
//!    freshly-minted child File cap.
//! 4. **Stat file:** confirm the new node reads back as an empty `File` → emit
//!    `0x5C7E` (`fs-create-stat`).

#![no_std]
#![no_main]

use fs_core::{NodeKind, Stat};
use fs_proto::{Op, Request, Response, UserBuf};
use snitchos_user::{endpoint, entry, telemetry, Endpoint};

/// Stat `cap` and return the decoded `Stat`, or `None` on any failure.
fn stat(cap: Endpoint) -> Option<Stat> {
    let (words, _) = cap.call(Request::Stat.encode()).ok()?;
    match Response::decode(Op::Stat, words) {
        Ok(Response::Stat(s)) => Some(s),
        _ => None,
    }
}

#[entry]
fn main() {
    // Connect → root directory File cap.
    let Ok((_r, Some(root_cap))) = endpoint().call([0, 0, 0, 0]) else {
        return;
    };
    let root = Endpoint::from_raw_handle(root_cap);

    // Stat the root: expect an empty Dir.
    if let Some(s) = stat(root)
        && s.kind == NodeKind::Dir
        && s.size == 0
    {
        let _ = telemetry().emit(0x57A7);
    }

    // Create "data" under the root → a child File cap in the reply.
    let name = b"data";
    let create = Request::Create {
        name: UserBuf { ptr: name.as_ptr() as u64, len: name.len() as u64 },
        kind: NodeKind::File,
    };
    let Ok((_c, Some(file_cap))) = root.call(create.encode()) else {
        return;
    };
    let file = Endpoint::from_raw_handle(file_cap);

    // Stat the new file: expect an empty File.
    if let Some(s) = stat(file)
        && s.kind == NodeKind::File
        && s.size == 0
    {
        let _ = telemetry().emit(0x5C7E);
    }

    // Write "hi" (data rides in via CopyFromCaller), then read it back (out via
    // CopyToCaller) and confirm the bytes survived the round-trip both ways.
    let data = b"hi";
    let write = Request::Write {
        offset: 0,
        src: UserBuf { ptr: data.as_ptr() as u64, len: data.len() as u64 },
    };
    let Ok((_w, _)) = file.call(write.encode()) else {
        return;
    };

    let mut buf = [0u8; 2];
    let read = Request::Read {
        offset: 0,
        dst: UserBuf { ptr: buf.as_mut_ptr() as u64, len: buf.len() as u64 },
    };
    let Ok((words, _)) = file.call(read.encode()) else {
        return;
    };
    if let Ok(Response::Count(n)) = Response::decode(Op::Read, words)
        && n == 2
        && buf == *b"hi"
    {
        let _ = telemetry().emit(0x317E);
    }
}
