//! `workload=manifest-satisfy`: the **generic satisfier**. Reads a child's
//! declared authority `needs` off the seeded FS (its `user.iface` xattr), matches
//! them against the caps *this* program holds via `hitch::satisfy`, and
//! `SpawnImage`s the child with the granted handles in slot order. The delegation
//! is **data-driven** — read from the child's manifest, not hardcoded — and each
//! satisfied slot is bracketed in a `satisfy.<role>` span (the named grant record;
//! the kernel's `CapEvent::Transferred` carries the cap id). The child then reads
//! its cap by that role name via `bootstrap().get`.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::vec::Vec;

use fs_proto::{FileRights, Op, Request, Response, UserBuf, XattrKey};
use hitch::{CapView, Grant, Unsatisfied, decode_manifest, satisfy};
use snitchos_user::{Endpoint, endpoint, entry, exit, object_kind, rights, spawn_image, tracer};

/// Path-walk the FS to a file cap (attach → `Lookup` each `/`-component).
fn cap_for(path: &str) -> Option<Endpoint> {
    let (_r, root) = endpoint().call([0, 0, 0, 0]).ok()?;
    let mut cap = Endpoint::from_raw_handle(root?);
    for part in path.split('/').filter(|p| !p.is_empty()) {
        let pb = part.as_bytes();
        let lookup = Request::Lookup {
            name: UserBuf { ptr: pb.as_ptr() as u64, len: pb.len() as u64 },
            rights: FileRights::READ,
        };
        let (_l, next) = cap.call(lookup.encode()).ok()?;
        cap = Endpoint::from_raw_handle(next?);
    }
    Some(cap)
}

/// The `user.iface` xattr behind `file` — the child's `hitch`-encoded manifest.
fn read_iface(file: Endpoint) -> Option<Vec<u8>> {
    let mut buf = [0u8; 256];
    let get = Request::GetXattr {
        key: XattrKey::UserIface,
        dst: UserBuf { ptr: buf.as_mut_ptr() as u64, len: buf.len() as u64 },
    };
    let (words, _) = file.call(get.encode()).ok()?;
    match Response::decode(Op::GetXattr, words) {
        Ok(Response::Count(n)) => Some(buf[..n as usize].to_vec()),
        _ => None,
    }
}

/// The whole file behind `file` (the child ELF), in 256-byte chunks.
fn read_all(file: Endpoint) -> Option<Vec<u8>> {
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

/// Satisfy `child`'s declared needs from `have` and `SpawnImage` it — or, if a
/// required slot can't be matched, **refuse** (no partial spawn), snitching a
/// `satisfy.refused.<role>` span. Each satisfied slot is bracketed in a
/// `satisfy.<role>` span (the named grant record).
fn process(child: &str, have: &[CapView]) {
    // The child's file cap — its manifest xattr and its ELF both live here.
    let Some(file) = cap_for(child) else { return };
    let Some(iface) = read_iface(file) else { return };
    let Ok(manifest) = decode_manifest(&iface) else { return };

    // Match the child's declared needs to our caps — the generic, data-driven
    // delegation. All-or-nothing: an unsatisfiable required slot refuses the whole
    // spawn, snitching the role rather than granting a partial set.
    let plan = match satisfy(&manifest.needs, have) {
        Ok(plan) => plan,
        Err(Unsatisfied { slot }) => {
            let role = manifest.needs.get(slot).map_or("?", |s| s.name.as_str());
            let _refused = tracer().span(&format!("satisfy.refused.{role}"));
            return;
        }
    };

    // Assemble the delegated-handle array in slot order, bracketing each grant in
    // a `satisfy.<role>` span — the named grant record.
    let mut handles: Vec<u32> = Vec::with_capacity(plan.len());
    for (slot, grant) in manifest.needs.iter().zip(&plan) {
        let _grant = tracer().span(&format!("satisfy.{}", slot.name));
        let handle = match grant {
            Grant::Use { handle } => *handle,
            Grant::Mint { from, rights } => {
                match Endpoint::from_raw_handle(*from as usize).mint_badged(0, *rights) {
                    Ok(h) => h as u32,
                    Err(_) => return,
                }
            }
        };
        handles.push(handle);
    }

    // Read the child ELF and spawn it with exactly the satisfied handles.
    let Some(elf) = read_all(file) else { return };
    let _ = spawn_image(&elf, &handles);
}

#[entry]
fn main() {
    // The caps *we* hold to satisfy from: our FS endpoint (a bare `SEND` cap
    // delivered at the startup endpoint). A real satisfier would enumerate its
    // table via `CapList`; here we know our one bootstrap cap.
    let have = [CapView {
        object: object_kind::ENDPOINT as u8,
        rights: rights::SEND,
        handle: endpoint().raw_handle() as u32,
    }];

    // Satisfiable: `fs-probe` needs the `SEND` cap we hold → grant + spawn.
    process("bin/fs-probe", &have);
    // Unsatisfiable: `fs-hungry` needs `RECV` we don't hold → refuse (snitched).
    process("bin/fs-hungry", &have);
    exit();
}
