//! v0.10 FS server (`workload=fs`) — **step 2a: the connect handshake.**
//!
//! Holds `RECV | MINT` on the FS endpoint. A client `call`s on its bare
//! endpoint cap (`badge == 0` = "attach"); the server mints a **root File cap**
//! — a badged `SEND` cap stamped `pack(root_inode, READ)` — and hands it back
//! via `reply_with_cap`. The kernel/init never compute this badge; the FS is
//! the sole minter (`docs/filesystem-design.md`). Request dispatch (`Stat`, …)
//! over the minted cap lands in step 2b.

#![no_std]
#![no_main]

use fs_core::InodeId;
use fs_proto::{Badge, FileRights};
use snitchos_user::{endpoint, entry, reply_with_cap, rights};

#[entry]
fn main() {
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
        }
        // r.badge != 0 (FS requests against a File cap): step 2b.
    }
}
