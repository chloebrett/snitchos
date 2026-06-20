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
//! 5. **Write/read:** round-trip bytes across the boundary → emit `0x317E`
//!    (`fs-write-read`).
//! 6. **Rights gate:** `lookup` the file asking READ-only and try to write
//!    through that attenuated cap — the FS refuses + snitches `snitchos.fs.denied`;
//!    a READ|WRITE lookup then writes successfully → emit `0x600D`
//!    (`fs-lookup-rights-gate`).

#![no_std]
#![no_main]

use fs_core::{FsError, NodeKind, Stat};
use fs_proto::{FileRights, Op, Request, Response, UserBuf};
use snitchos_user::{Endpoint, endpoint, entry, telemetry};

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
        name: UserBuf {
            ptr: name.as_ptr() as u64,
            len: name.len() as u64,
        },
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
        src: UserBuf {
            ptr: data.as_ptr() as u64,
            len: data.len() as u64,
        },
    };
    let Ok((_w, _)) = file.call(write.encode()) else {
        return;
    };

    let mut buf = [0u8; 2];
    let read = Request::Read {
        offset: 0,
        dst: UserBuf {
            ptr: buf.as_mut_ptr() as u64,
            len: buf.len() as u64,
        },
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

    // Rights gate: look the file up asking for READ only — the server mints an
    // attenuated `(file, READ)` cap — then try to write through it. The FS gate
    // must refuse it and snitch `snitchos.fs.denied`; the client only triggers
    // it (the refusal is observed server-side, not here).
    let lookup_name = UserBuf {
        ptr: name.as_ptr() as u64,
        len: name.len() as u64,
    };
    let write_hi = Request::Write {
        offset: 0,
        src: UserBuf {
            ptr: data.as_ptr() as u64,
            len: data.len() as u64,
        },
    };
    if let Ok((_l, Some(ro))) = root.call(
        Request::Lookup {
            name: lookup_name,
            rights: FileRights::READ,
        }
        .encode(),
    ) {
        let _ = Endpoint::from_raw_handle(ro).call(write_hi.encode());
    }

    // Positive control: look the same file up asking for READ|WRITE; the write
    // through *that* cap must succeed — proving the gate refuses only the
    // under-authorized write, not every write.
    if let Ok((_l, Some(rw))) = root.call(
        Request::Lookup {
            name: lookup_name,
            rights: FileRights::READ | FileRights::WRITE,
        }
        .encode(),
    ) {
        if let Ok((words, _)) = Endpoint::from_raw_handle(rw).call(write_hi.encode())
            && matches!(Response::decode(Op::Write, words), Ok(Response::Count(_)))
        {
            let _ = telemetry().emit(0x600D);
        }
    }

    // Remove the file, then confirm the name no longer resolves — proving the
    // unlink took effect across the boundary, not just that the server replied.
    if let Ok((rm, _)) = root.call(Request::Remove { name: lookup_name }.encode())
        && matches!(Response::decode(Op::Remove, rm), Ok(Response::Removed))
        && let Ok((gone, _)) = root.call(
            Request::Lookup {
                name: lookup_name,
                rights: FileRights::READ,
            }
            .encode(),
        )
        && matches!(
            Response::decode(Op::Lookup, gone),
            Ok(Response::Err(FsError::NotFound))
        )
    {
        let _ = telemetry().emit(0xDE1E);
    }
}
