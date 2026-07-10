//! The v0.10 FS server's receive loop, factored out of the binary so both the
//! plain server (`fs-server`, an empty [`RamFs`](ramfs::RamFs)) and the seeded
//! server (`fs-server-seeded`, a `RamFs::seeded` from the build-time fs-image)
//! share one implementation. Generic over [`Filesystem`] — the binary picks the
//! concrete fs.
//!
//! Holds `RECV | MINT` on the FS endpoint. Two kinds of caller, demuxed by the
//! kernel-delivered badge:
//! - `badge == 0` (a client's bare cap = "attach") → mint a **root File cap**
//!   and hand it back via `reply_with_cap`. The FS is the sole minter; the
//!   kernel never reads the badge's meaning (`docs/filesystem-design.md`).
//! - `badge != 0` (a File cap) → unpack `(inode, rights)`, decode the request,
//!   run it against the trait, and `reply`.

#![no_std]

use fs_core::{Filesystem, FsError, InodeId};
use fs_proto::{Badge, Denial, FileRights, Request, Response, XattrKey, check_rights};
use snitchos_user::{
    Endpoint, Metric, copy_from_caller, copy_to_caller, delegated_handle, register_gauge, reply,
    reply_with_cap, rights, tracer,
};

/// This server's FS endpoint, read as the first **delegated** cap (handle 2).
/// Works whichever way the server was launched: the kernel `run_ipc` path puts
/// the endpoint at handle 2 (after the two bootstrap caps), and an init-`Spawn`
/// delegating the endpoint lands it at the same `delegated_handle(0)` — so the
/// server no longer depends on the legacy `a2` startup-endpoint slot.
fn fs_endpoint() -> Endpoint {
    Endpoint::from_raw_handle(delegated_handle(0))
}

/// Largest filename the server will pull across in one `create` (≤ the kernel's
/// per-copy cap). Names longer than this are refused.
const NAME_CAP: usize = 64;

/// Server-side scratch for one `read`/`write` payload (≤ the kernel's per-copy
/// cap). Larger transfers are the client's job to chunk by offset.
const DATA_CAP: usize = 256;

/// Serve `fs` over this process's FS endpoint forever. The binary supplies the
/// concrete filesystem (empty or seeded).
pub fn serve<F: Filesystem>(mut fs: F) -> ! {
    // The FS names its own denial metric: the kernel no longer knows
    // `snitchos.fs.denied` ahead of time — the server registers it (through its
    // startup `TelemetrySink`) and emits the structured denial through the
    // returned handle. An inert `Metric` if registration was refused (it isn't
    // here); its `emit` is then a harmless no-op.
    let denied: Metric = register_gauge("snitchos.fs.denied");
    loop {
        let Ok(r) = fs_endpoint().receive_with_reply() else {
            continue;
        };
        let Some(reply_handle) = r.reply else {
            continue; // one-way send: no request semantics yet
        };
        if r.badge == 0 {
            // Attach: mint the root File cap and transfer it to the caller.
            let badge = Badge {
                inode: InodeId::new(0),
                rights: FileRights::READ | FileRights::WRITE,
            }
            .pack();
            if let Ok(cap) = fs_endpoint().mint_badged(badge, rights::SEND) {
                let _ = reply_with_cap(reply_handle, [0, 0, 0, 0], cap);
            }
            continue;
        }
        // A File cap: the badge names the inode and the file rights granted on
        // it. Decode the request, then run the rights gate — refusals snitch.
        let badge = Badge::unpack(r.badge);
        let Ok(req) = Request::decode(r.msg) else {
            let _ = reply(reply_handle, Response::Err(FsError::Internal).encode());
            continue;
        };
        // Each request is a span. The kernel seeded our span cursor with the
        // caller's open op span on `receive`, so this nests *under* the client's
        // `fs.<op>` — the trace crosses the process boundary for free.
        let _serve = tracer().span("fs.serve");
        // The gate the kernel cannot run: it carries the badge's file rights but
        // never interprets them. On refusal, snitch the structured `(inode,
        // attempted)` to the denial gauge, then reply `Denied` — never silent.
        if let Err(attempted) = check_rights(req.op(), badge.rights) {
            denied.emit(Denial { inode: badge.inode, attempted }.pack());
            let _ = reply(reply_handle, Response::Err(FsError::Denied).encode());
            continue;
        }
        match req {
            Request::Stat => {
                let resp = match fs.stat(badge.inode) {
                    Ok(s) => Response::Stat(s),
                    Err(e) => Response::Err(e),
                };
                let _ = reply(reply_handle, resp.encode());
            }
            Request::Create { name, kind } => {
                create(&mut fs, reply_handle, badge, name, kind);
            }
            Request::Lookup { name, rights } => {
                lookup(&mut fs, reply_handle, badge, name, rights);
            }
            Request::Write { offset, src } => {
                // Pull the caller's data across, then write it into the file.
                let mut scratch = [0u8; DATA_CAP];
                let resp = match copy_from_caller(reply_handle, src.ptr as usize, src.len as usize, scratch.as_mut_ptr() as usize) {
                    Ok(n) => match fs.write(badge.inode, offset, &scratch[..n]) {
                        Ok(written) => Response::Count(written as u64),
                        Err(e) => Response::Err(e),
                    },
                    Err(_) => Response::Err(FsError::Internal),
                };
                let _ = reply(reply_handle, resp.encode());
            }
            Request::Read { offset, dst } => {
                // Read into scratch, then push it out to the caller's buffer.
                let mut scratch = [0u8; DATA_CAP];
                let want = (dst.len as usize).min(DATA_CAP);
                let resp = match fs.read(badge.inode, offset, &mut scratch[..want]) {
                    Ok(n) => match copy_to_caller(reply_handle, scratch.as_ptr() as usize, n, dst.ptr as usize) {
                        Ok(_) => Response::Count(n as u64),
                        Err(_) => Response::Err(FsError::Internal),
                    },
                    Err(e) => Response::Err(e),
                };
                let _ = reply(reply_handle, resp.encode());
            }
            Request::Remove { name } => {
                remove(&mut fs, reply_handle, badge, name);
            }
            Request::Readdir { index, name_dst } => {
                readdir(&mut fs, reply_handle, badge, index, name_dst);
            }
            Request::GetXattr { key, dst } => {
                get_xattr(&mut fs, reply_handle, badge, key, dst);
            }
            Request::Truncate { len } => {
                // The rights gate above already refused this without WRITE. Resize
                // the badged file; reply with the new length as the count.
                let resp = match fs.truncate(badge.inode, len) {
                    Ok(()) => Response::Count(len),
                    Err(e) => Response::Err(e),
                };
                let _ = reply(reply_handle, resp.encode());
            }
        }
    }
}

/// Handle `getxattr`: read the inode's attribute and push its bytes to the
/// caller's buffer (option-D copy), replying with the count copied. The value
/// (e.g. a `user.iface` manifest) is the source directly — no scratch buffer, so
/// it isn't capped at `DATA_CAP` like the file-read path.
fn get_xattr<F: Filesystem>(
    fs: &mut F,
    reply_handle: usize,
    file: Badge,
    key: XattrKey,
    dst: fs_proto::UserBuf,
) {
    let resp = match fs.getxattr(file.inode, key.name()) {
        Ok(value) => {
            let n = (dst.len as usize).min(value.len());
            match copy_to_caller(reply_handle, value.as_ptr() as usize, n, dst.ptr as usize) {
                Ok(_) => Response::Count(n as u64),
                Err(_) => Response::Err(FsError::Internal),
            }
        }
        Err(e) => Response::Err(e),
    };
    let _ = reply(reply_handle, resp.encode());
}

/// Handle `create`: pull the filename across from the caller (option-D copy),
/// create the node, and — on success — mint a child File cap and transfer it
/// back in the reply. The child's rights are `dir.rights ∩ (READ|WRITE)` — a
/// minted cap never exceeds the authority of the directory cap it was minted
/// through. The filename rides the cross-AS copy rather than the inline message.
fn create<F: Filesystem>(
    fs: &mut F,
    reply_handle: usize,
    dir: Badge,
    name: fs_proto::UserBuf,
    kind: fs_core::NodeKind,
) {
    let created = with_name(reply_handle, name, |s| fs.create(dir.inode, s, kind));
    let child_rights = dir.rights & (FileRights::READ | FileRights::WRITE);
    reply_minted_child(reply_handle, created, child_rights);
}

/// Pull a filename from the caller (option-D copy) and hand it to `f`. The
/// shared name-copy prologue of `create`/`lookup`/`remove`: on a copy failure
/// or a non-UTF-8 name it yields `NameTooLong` without calling `f`.
fn with_name<R>(
    reply_handle: usize,
    name: fs_proto::UserBuf,
    f: impl FnOnce(&str) -> Result<R, FsError>,
) -> Result<R, FsError> {
    let mut buf = [0u8; NAME_CAP];
    copy_from_caller(reply_handle, name.ptr as usize, name.len as usize, buf.as_mut_ptr() as usize)
        .ok()
        .and_then(|n| core::str::from_utf8(&buf[..n]).ok())
        .map_or(Err(FsError::NameTooLong), f)
}

/// Handle `lookup`: pull the name across, resolve it to a child inode, and — on
/// success — mint a child File cap badged `dir.rights ∩ requested` and transfer
/// it back. This is the attenuation point: a client asks for the rights it
/// wants, the FS grants no more than the directory cap already carries.
fn lookup<F: Filesystem>(
    fs: &mut F,
    reply_handle: usize,
    dir: Badge,
    name: fs_proto::UserBuf,
    requested: FileRights,
) {
    let found = with_name(reply_handle, name, |s| fs.lookup(dir.inode, s));
    let child_rights = dir.rights & requested;
    reply_minted_child(reply_handle, found, child_rights);
}

/// Handle `remove`: pull the name across, unlink it from the directory, and
/// reply `Removed` (or the FS error). Ungated in the flat core — any cap to the
/// directory may remove (directory rights are a deferred follow-on).
fn remove<F: Filesystem>(fs: &mut F, reply_handle: usize, dir: Badge, name: fs_proto::UserBuf) {
    let resp = match with_name(reply_handle, name, |s| fs.remove(dir.inode, s)) {
        Ok(()) => Response::Removed,
        Err(e) => Response::Err(e),
    };
    let _ = reply(reply_handle, resp.encode());
}

/// Handle `readdir`: list the directory and return the entry at `index` — its
/// inode + kind inline, its name copied out into the caller's `name_dst` buffer
/// (option-D `CopyToCaller`). An `index` past the last entry replies `NotFound`,
/// the client's end-of-list signal. Ungated (a metadata op).
fn readdir<F: Filesystem>(fs: &mut F, reply_handle: usize, dir: Badge, index: u64, name_dst: fs_proto::UserBuf) {
    let resp = match fs.readdir(dir.inode) {
        Ok(entries) => match entries.get(index as usize) {
            Some(entry) => {
                let name = entry.name.as_bytes();
                let n = name.len().min(name_dst.len as usize);
                match copy_to_caller(reply_handle, name.as_ptr() as usize, n, name_dst.ptr as usize) {
                    Ok(_) => Response::Entry { ino: entry.ino, kind: entry.kind, name_len: n as u64 },
                    Err(_) => Response::Err(FsError::Internal),
                }
            }
            None => Response::Err(FsError::NotFound),
        },
        Err(e) => Response::Err(e),
    };
    let _ = reply(reply_handle, resp.encode());
}

/// Reply to a `create`/`lookup`: on a resolved `child` inode, mint a File cap
/// badged `(child, child_rights)` and transfer it via `reply_with_cap`; on an
/// FS error, reply the error. The shared tail of both cap-minting ops.
fn reply_minted_child(reply_handle: usize, child: Result<InodeId, FsError>, child_rights: FileRights) {
    match child {
        Ok(child) => {
            let badge = Badge { inode: child, rights: child_rights }.pack();
            match fs_endpoint().mint_badged(badge, rights::SEND) {
                Ok(cap) => {
                    let _ = reply_with_cap(reply_handle, Response::Inode(child).encode(), cap);
                }
                Err(_) => {
                    let _ = reply(reply_handle, Response::Err(FsError::Internal).encode());
                }
            }
        }
        Err(e) => {
            let _ = reply(reply_handle, Response::Err(e).encode());
        }
    }
}
