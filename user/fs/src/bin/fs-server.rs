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
use fs_proto::{Badge, Request, Response};
use ramfs::RamFs;
use snitchos_user::{endpoint, entry, reply, reply_with_cap, rights};

#[entry]
fn main() {
    let fs = RamFs::new();
    loop {
        let Ok(r) = endpoint().receive_with_reply() else {
            continue;
        };
        let Some(reply_handle) = r.reply else {
            continue; // one-way send: no request semantics yet
        };
        if r.badge == 0 {
            // Attach: mint the root File cap and transfer it to the caller.
            let badge = Badge { inode: InodeId::new(0), rights: fs_proto::FileRights::READ }.pack();
            if let Ok(cap) = endpoint().mint_badged(badge, rights::SEND) {
                let _ = reply_with_cap(reply_handle, [0, 0, 0, 0], cap);
            }
            continue;
        }
        // A File cap: the badge names the inode (and its file rights).
        let inode = Badge::unpack(r.badge).inode;
        let resp = match Request::decode(r.msg) {
            Ok(Request::Stat) => match fs.stat(inode) {
                Ok(s) => Response::Stat(s),
                Err(e) => Response::Err(e),
            },
            // Payload-bearing ops need the option-D copy primitive (step 4).
            Ok(_) => Response::Err(FsError::Unsupported),
            // A malformed request is, for now, an unsupported one.
            Err(_) => Response::Err(FsError::Unsupported),
        };
        let _ = reply(reply_handle, resp.encode());
    }
}
