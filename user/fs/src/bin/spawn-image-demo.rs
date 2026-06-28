//! `workload=spawn-image`: exercises the `SpawnImage` syscall — spawning a
//! process from a **caller-supplied ELF read off the filesystem**, vs the
//! kernel-embedded `Spawn` registry.
//!
//! Two checks:
//! - **negative:** a malformed image must be *refused* (the kernel snitches a
//!   `SyscallRefused` for SpawnImage), never crash the kernel.
//! - **positive:** read the real `spawnee` ELF from `/bin/spawnee` and spawn it,
//!   delegating our span cap so the child can open its `spawnee.via_delegated`
//!   span through the delegated handle — proving the image loaded, ran, and the
//!   delegation arrived.

#![no_std]
#![no_main]

extern crate alloc;
use alloc::vec::Vec;

use fs_proto::{FileRights, Op, Request, Response, UserBuf};
use snitchos_user::{Endpoint, endpoint, entry, exit, span_handle, spawn_image};

/// Read a whole file off the fs endpoint, path-walking the `/`-components.
fn read_file(path: &str) -> Option<Vec<u8>> {
    let (_r, root_cap) = endpoint().call([0, 0, 0, 0]).ok()?;
    let mut cap = Endpoint::from_raw_handle(root_cap?);
    for part in path.split('/').filter(|p| !p.is_empty()) {
        let pb = part.as_bytes();
        let lookup = Request::Lookup {
            name: UserBuf { ptr: pb.as_ptr() as u64, len: pb.len() as u64 },
            rights: FileRights::READ,
        };
        let (_l, next) = cap.call(lookup.encode()).ok()?;
        cap = Endpoint::from_raw_handle(next?);
    }
    let file = cap;
    let mut bytes = Vec::new();
    let mut offset = 0u64;
    let mut chunk = [0u8; 256];
    loop {
        let read = Request::Read {
            offset,
            dst: UserBuf { ptr: chunk.as_mut_ptr() as u64, len: chunk.len() as u64 },
        };
        let (words, _) = file.call(read.encode()).ok()?;
        let n = match Response::decode(Op::Read, words) {
            Ok(Response::Count(n)) => n as usize,
            _ => break,
        };
        if n == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..n]);
        offset += n as u64;
        if n < chunk.len() {
            break;
        }
    }
    Some(bytes)
}

#[entry]
fn main() {
    // Negative: a malformed ELF must be refused (the kernel snitches a
    // `SyscallRefused`), not crash. We don't act on the `None` — the refusal
    // frame is the signal.
    let _ = spawn_image(&[0xde, 0xad, 0xbe, 0xef], &[]);

    // Positive: read the real `spawnee` ELF off the filesystem and spawn it,
    // delegating our span cap (handle 1) so the child can open its
    // `spawnee.via_delegated` span through the delegated handle.
    if let Some(elf) = read_file("bin/spawnee") {
        let _ = spawn_image(&elf, &[span_handle()]);
    }

    exit();
}
