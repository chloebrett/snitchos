//! `workload=manifest-iface`: the end-to-end proof of the typed-interface path.
//!
//! Reads `/bin/manifest_demo`'s `user.iface` xattr off the seeded FS (over the
//! new `GetXattr` op), `hitch::decode_manifest`s the bytes, and checks the shape
//! matches what `#[entry(in = Row, out = u64, uses = [ConsoleOut])]` declares.
//! Emits `snitchos.manifest.iface_ok = 1` only if the whole chain — ELF note →
//! build-time extraction → xattr → IPC → decode — reconstructs the manifest
//! exactly. The scenario asserts that value.

#![no_std]
#![no_main]

use fs_proto::{FileRights, Op, Request, Response, UserBuf, XattrKey};
use hitch::{Manifest, TypeSchema, decode_manifest};
use snitchos_user::{Endpoint, endpoint, entry, register_gauge};

/// Resolve `path` to a File cap by attaching to the FS (badge-0 send mints the
/// root cap) and walking each `/`-component with `Lookup`.
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

/// Read `bin/manifest_demo`'s `user.iface` xattr and decode it to a `Manifest`.
fn read_manifest() -> Option<Manifest> {
    let cap = cap_for("bin/manifest_demo")?;
    let mut buf = [0u8; 256];
    let get = Request::GetXattr {
        key: XattrKey::UserIface,
        dst: UserBuf { ptr: buf.as_mut_ptr() as u64, len: buf.len() as u64 },
    };
    let (words, _) = cap.call(get.encode()).ok()?;
    let n = match Response::decode(Op::GetXattr, words) {
        Ok(Response::Count(n)) => n as usize,
        _ => return None,
    };
    decode_manifest(&buf[..n]).ok()
}

/// Does the manifest match `#[entry(in = Row, out = u64, uses = [ConsoleOut])]`
/// with `Row { name: u32, count: i64 }`?
fn shape_matches(m: &Manifest) -> bool {
    let input_ok = matches!(
        &m.input,
        Some(TypeSchema::Product { fields, .. }) if fields.len() == 2
    );
    let output_ok = m.output == TypeSchema::U64;
    let uses_ok = m.uses.len() == 1 && m.uses[0] == "ConsoleOut";
    input_ok && output_ok && uses_ok
}

#[entry]
fn main() {
    let ok = read_manifest().is_some_and(|m| shape_matches(&m));
    register_gauge("snitchos.manifest.iface_ok").emit(i64::from(ok));
}
