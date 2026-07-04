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
use snitchos_user::{Denied, Endpoint, MSG_WORDS, Metric, delegated_handle, entry, register_counter, tracer};

/// `call` `req` on `cap` inside a span named `span` — the span stays open across
/// the `call`, so the server's handling nests under it across the process
/// boundary. The single-call idiom every request below shares (readdir spans two
/// calls, so it keeps its own block).
fn traced_call(
    cap: Endpoint,
    span: &str,
    req: [u64; MSG_WORDS],
) -> Result<([u64; MSG_WORDS], Option<usize>), Denied> {
    let _s = tracer().span(span);
    cap.call(req)
}

/// Stat `cap` and return the decoded `Stat`, or `None` on any failure.
fn stat(cap: Endpoint) -> Option<Stat> {
    let (words, _) = traced_call(cap, "fs.stat", Request::Stat.encode()).ok()?;
    match Response::decode(Op::Stat, words) {
        Ok(Response::Stat(s)) => Some(s),
        _ => None,
    }
}

#[entry]
fn main() {
    // The client names its own checkpoint metric; each step emits its marker
    // value through this handle (one counter, many values — as before, but
    // process-named rather than the shared `telemetry_total`).
    let marker: Metric = register_counter("snitchos.fs_client.marker");

    // Connect → root directory File cap. The FS endpoint is our first delegated
    // cap (handle 2) — works whether launched by `run_ipc` (endpoint at handle 2)
    // or by an init-`Spawn` delegating a bare `SEND` cap (delegated[0]).
    let fs = Endpoint::from_raw_handle(delegated_handle(0));
    let Ok((_r, Some(root_cap))) = fs.call([0, 0, 0, 0]) else {
        return;
    };
    let root = Endpoint::from_raw_handle(root_cap);

    // Stat the root: expect an empty Dir.
    if let Some(s) = stat(root)
        && s.kind == NodeKind::Dir
        && s.size == 0
    {
        marker.emit(markers::STAT_ROOT_OK);
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
    let Ok((_c, Some(file_cap))) = traced_call(root, "fs.create", create.encode()) else {
        return;
    };
    let file = Endpoint::from_raw_handle(file_cap);

    // Stat the new file: expect an empty File.
    if let Some(s) = stat(file)
        && s.kind == NodeKind::File
        && s.size == 0
    {
        marker.emit(markers::CREATE_STAT_OK);
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
    let Ok((_w, _)) = traced_call(file, "fs.write", write.encode()) else {
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
    let Ok((words, _)) = traced_call(file, "fs.read", read.encode()) else {
        return;
    };
    if let Ok(Response::Count(n)) = Response::decode(Op::Read, words)
        && n == 2
        && buf == *b"hi"
    {
        marker.emit(markers::WRITE_READ_OK);
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
            marker.emit(markers::READDIR_OK);
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
    let ro = traced_call(
        root,
        "fs.lookup",
        Request::Lookup { name: lookup_name, rights: FileRights::READ }.encode(),
    );
    if let Ok((_l, Some(ro))) = ro {
        let _ = traced_call(Endpoint::from_raw_handle(ro), "fs.write", write_hi.encode());
    }

    // Positive control: look the same file up asking for READ|WRITE; the write
    // through *that* cap must succeed — proving the gate refuses only the
    // under-authorized write, not every write.
    let rw = traced_call(
        root,
        "fs.lookup",
        Request::Lookup { name: lookup_name, rights: FileRights::READ | FileRights::WRITE }.encode(),
    );
    if let Ok((_l, Some(rw))) = rw
        && let Ok((words, _)) = traced_call(Endpoint::from_raw_handle(rw), "fs.write", write_hi.encode())
        && matches!(Response::decode(Op::Write, words), Ok(Response::Count(_)))
    {
        marker.emit(markers::WRITE_AUTHORIZED_OK);
    }

    // Remove the file, then confirm the name no longer resolves — proving the
    // unlink took effect across the boundary, not just that the server replied.
    let rm = traced_call(root, "fs.remove", Request::Remove { name: lookup_name }.encode());
    if let Ok((rm, _)) = rm
        && matches!(Response::decode(Op::Remove, rm), Ok(Response::Removed))
    {
        let gone = traced_call(
            root,
            "fs.lookup",
            Request::Lookup { name: lookup_name, rights: FileRights::READ }.encode(),
        );
        if let Ok((gone, _)) = gone
            && matches!(Response::decode(Op::Lookup, gone), Ok(Response::Err(FsError::NotFound)))
        {
            marker.emit(markers::REMOVE_OK);
        }
    }
}
