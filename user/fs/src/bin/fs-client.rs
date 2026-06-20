//! v0.10 FS client (`workload=fs`) — exercises the FS over capabilities:
//!
//! 1. **Connect:** `call` on the bare endpoint cap (`badge == 0` = "attach");
//!    the server mints `pack(root, READ)` and returns it.
//! 2. **Stat root:** confirm the root reads back as an empty `Dir` → emit
//!    [`markers::STAT_ROOT_OK`] (`fs-stat-root`).
//! 3. **Create:** `call` `Request::Create` on the root cap — the filename rides
//!    as a `UserBuf` the kernel copies across (option D) — and receive the
//!    freshly-minted child File cap.
//! 4. **Stat file:** confirm the new node reads back as an empty `File` → emit
//!    [`markers::CREATE_STAT_OK`] (`fs-create-stat`).
//! 5. **Write/read:** round-trip bytes across the boundary → emit
//!    [`markers::WRITE_READ_OK`] (`fs-write-read`).
//! 6. **Rights gate:** `lookup` the file asking READ-only and try to write
//!    through that attenuated cap — the FS refuses + snitches `snitchos.fs.denied`;
//!    a READ|WRITE lookup then writes successfully → emit
//!    [`markers::WRITE_AUTHORIZED_OK`] (`fs-lookup-rights-gate`).

#![no_std]
#![no_main]

use fs_core::{FsError, InodeId, NodeKind, Stat};
use fs_proto::{markers, FileRights, Op, Request, Response, UserBuf};
use snitchos_user::{Endpoint, endpoint, entry, telemetry, tracer};

/// Stat `cap` and return the decoded `Stat`, or `None` on any failure. The
/// `fs.stat` span stays open across the `call`, so the server's handling nests
/// under it across the process boundary.
fn stat(cap: Endpoint) -> Option<Stat> {
    let _s = tracer().span("fs.stat");
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
        let _ = telemetry().emit(markers::STAT_ROOT_OK);
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
    let file_cap = {
        let _s = tracer().span("fs.create");
        let Ok((_c, Some(cap))) = root.call(create.encode()) else {
            return;
        };
        cap
    };
    let file = Endpoint::from_raw_handle(file_cap);

    // Stat the new file: expect an empty File.
    if let Some(s) = stat(file)
        && s.kind == NodeKind::File
        && s.size == 0
    {
        let _ = telemetry().emit(markers::CREATE_STAT_OK);
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
    {
        let _s = tracer().span("fs.write");
        let Ok((_w, _)) = file.call(write.encode()) else {
            return;
        };
    }

    let mut buf = [0u8; 2];
    let read = Request::Read {
        offset: 0,
        dst: UserBuf {
            ptr: buf.as_mut_ptr() as u64,
            len: buf.len() as u64,
        },
    };
    let words = {
        let _s = tracer().span("fs.read");
        let Ok((words, _)) = file.call(read.encode()) else {
            return;
        };
        words
    };
    if let Ok(Response::Count(n)) = Response::decode(Op::Read, words)
        && n == 2
        && buf == *b"hi"
    {
        let _ = telemetry().emit(markers::WRITE_READ_OK);
    }

    // List the root directory: readdir(0) returns the one entry ("data"), its
    // name copied out via CopyToCaller; readdir(1) reports end-of-list. Runs
    // while "data" still exists — the remove below empties the directory.
    let mut entry_name = [0u8; 8];
    let name_dst = UserBuf {
        ptr: entry_name.as_mut_ptr() as u64,
        len: entry_name.len() as u64,
    };
    {
        let _s = tracer().span("fs.readdir");
        if let Ok((e0, _)) = root.call(Request::Readdir { index: 0, name_dst }.encode())
            && let Ok(Response::Entry {
                ino,
                kind,
                name_len,
            }) = Response::decode(Op::Readdir, e0)
            && ino == InodeId::new(1)
            && kind == NodeKind::File
            && name_len == 4
            && entry_name[..4] == *b"data"
            && let Ok((e1, _)) = root.call(Request::Readdir { index: 1, name_dst }.encode())
            && matches!(
                Response::decode(Op::Readdir, e1),
                Ok(Response::Err(FsError::NotFound))
            )
        {
            let _ = telemetry().emit(markers::READDIR_OK);
        }
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
    let ro = {
        let _s = tracer().span("fs.lookup");
        root.call(Request::Lookup { name: lookup_name, rights: FileRights::READ }.encode())
    };
    if let Ok((_l, Some(ro))) = ro {
        let _s = tracer().span("fs.write");
        let _ = Endpoint::from_raw_handle(ro).call(write_hi.encode());
    }

    // Positive control: look the same file up asking for READ|WRITE; the write
    // through *that* cap must succeed — proving the gate refuses only the
    // under-authorized write, not every write.
    let rw = {
        let _s = tracer().span("fs.lookup");
        root.call(
            Request::Lookup { name: lookup_name, rights: FileRights::READ | FileRights::WRITE }.encode(),
        )
    };
    if let Ok((_l, Some(rw))) = rw {
        let _s = tracer().span("fs.write");
        if let Ok((words, _)) = Endpoint::from_raw_handle(rw).call(write_hi.encode())
            && matches!(Response::decode(Op::Write, words), Ok(Response::Count(_)))
        {
            let _ = telemetry().emit(markers::WRITE_AUTHORIZED_OK);
        }
    }

    // Remove the file, then confirm the name no longer resolves — proving the
    // unlink took effect across the boundary, not just that the server replied.
    let rm = {
        let _s = tracer().span("fs.remove");
        root.call(Request::Remove { name: lookup_name }.encode())
    };
    if let Ok((rm, _)) = rm
        && matches!(Response::decode(Op::Remove, rm), Ok(Response::Removed))
    {
        let gone = {
            let _s = tracer().span("fs.lookup");
            root.call(Request::Lookup { name: lookup_name, rights: FileRights::READ }.encode())
        };
        if let Ok((gone, _)) = gone
            && matches!(Response::decode(Op::Lookup, gone), Ok(Response::Err(FsError::NotFound)))
        {
            let _ = telemetry().emit(markers::REMOVE_OK);
        }
    }
}
