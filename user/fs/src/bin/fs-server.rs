//! v0.10 FS server (`workload=fs`).
//!
//! Holds `RECV | MINT` on the FS endpoint and serves a flat in-memory
//! [`RamFs`]. Two kinds of caller, demuxed by the kernel-delivered badge:
//! - `badge == 0` (a client's bare cap = "attach") → mint a **root File cap**
//!   (badged `SEND` stamped `pack(root_inode, READ)`) and hand it back via
//!   `reply_with_cap`. The FS is the sole minter; the kernel never reads the
//!   badge's meaning (`docs/filesystem-design.md`).
//! - `badge != 0` (a File cap) → unpack `(inode, rights)`, decode the request,
//!   run it against the trait, and `reply`. (Step 2b serves `Stat`; the
//!   payload-bearing ops arrive with the option-D copy primitive.)

#![no_std]
#![no_main]

use fs_core::{Filesystem, FsError, InodeId};
use fs_proto::{Badge, FileRights, Request, Response};
use ramfs::RamFs;
use snitchos_user::{copy_from_caller, endpoint, entry, reply, reply_with_cap, rights};

/// Largest filename the server will pull across in one `create` (≤ the kernel's
/// per-copy cap). Names longer than this are refused.
const NAME_CAP: usize = 64;

#[entry]
fn main() {
    let mut fs = RamFs::new();
    loop {
        let Ok(r) = endpoint().receive_with_reply() else {
            continue;
        };
        let Some(reply_handle) = r.reply else {
            continue; // one-way send: no request semantics yet
        };
        if r.badge == 0 {
            // Attach: mint the root File cap and transfer it to the caller.
            let badge = Badge { inode: InodeId::new(0), rights: FileRights::READ }.pack();
            if let Ok(cap) = endpoint().mint_badged(badge, rights::SEND) {
                let _ = reply_with_cap(reply_handle, [0, 0, 0, 0], cap);
            }
            continue;
        }
        // A File cap: the badge names the inode (and its file rights).
        let inode = Badge::unpack(r.badge).inode;
        match Request::decode(r.msg) {
            Ok(Request::Stat) => {
                let resp = match fs.stat(inode) {
                    Ok(s) => Response::Stat(s),
                    Err(e) => Response::Err(e),
                };
                let _ = reply(reply_handle, resp.encode());
            }
            Ok(Request::Create { name, kind }) => {
                create(&mut fs, reply_handle, inode, name, kind);
            }
            // Remaining payload ops (read/write/lookup/remove/readdir): later steps.
            Ok(_) => {
                let _ = reply(reply_handle, Response::Err(FsError::Unsupported).encode());
            }
            Err(_) => {
                let _ = reply(reply_handle, Response::Err(FsError::Unsupported).encode());
            }
        }
    }
}

/// Handle `create`: pull the filename across from the caller (option-D copy),
/// create the node, and — on success — mint a child File cap (`READ|WRITE`) and
/// transfer it back in the reply. The filename is the first FS arg to ride the
/// cross-AS copy rather than the inline message.
fn create(
    fs: &mut RamFs,
    reply_handle: usize,
    dir: InodeId,
    name: fs_proto::UserBuf,
    kind: fs_core::NodeKind,
) {
    let mut buf = [0u8; NAME_CAP];
    let created = copy_from_caller(reply_handle, name.ptr as usize, name.len as usize, buf.as_mut_ptr() as usize)
        .ok()
        .and_then(|n| core::str::from_utf8(&buf[..n]).ok())
        .map_or(Err(FsError::NameTooLong), |s| fs.create(dir, s, kind));

    match created {
        Ok(child) => {
            let badge = Badge { inode: child, rights: FileRights::READ | FileRights::WRITE }.pack();
            match endpoint().mint_badged(badge, rights::SEND) {
                Ok(cap) => {
                    let _ = reply_with_cap(reply_handle, Response::Inode(child).encode(), cap);
                }
                Err(_) => {
                    let _ = reply(reply_handle, Response::Err(FsError::Unsupported).encode());
                }
            }
        }
        Err(e) => {
            let _ = reply(reply_handle, Response::Err(e).encode());
        }
    }
}
