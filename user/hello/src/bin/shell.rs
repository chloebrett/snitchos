//! Minimal powerbox shell (`workload=shell`): reads commands from the UART
//! console and executes them with least-authority. The only command today:
//!
//! ```text
//! view <path>
//! ```
//!
//! Looks up `<path>` on the FS with READ-only rights, spawns the viewer with
//! that attenuated cap, then revokes it when the viewer exits — the full
//! delegate → use → revoke cycle observable as CapEvents in Tempo.

#![no_std]
#![no_main]

use fs_proto::{FileRights, Request, UserBuf};
use snitchos_user::{Endpoint, bootstrap, console_read, entry, revoke, spawn, tracer, wait, yield_now};

/// SPAWNABLE index for the viewer binary (see `kernel/src/trap/user.rs`).
const VIEWER_ID: usize = 6;

#[entry(needs = [("fs", ENDPOINT, SEND)])]
fn main() {
    let Some(fs) = bootstrap().get::<Endpoint>("fs") else {
        return;
    };

    // Connect to FS root.
    let Ok((_r, Some(root_cap))) = fs.call([0, 0, 0, 0]) else {
        return;
    };
    let root = Endpoint::from_raw_handle(root_cap);

    // Signal readiness: the itest waits for this span before injecting input.
    let _alive = tracer().span("shell.ready");

    let mut buf = [0u8; 128];
    loop {
        let n = console_read(&mut buf);
        if n == 0 {
            continue;
        }
        let line = trim_newline(&buf[..n]);
        if let Some(path) = line.strip_prefix(b"view ") {
            cmd_view(root, path);
        }
    }
}

fn cmd_view(root: Endpoint, path: &[u8]) {
    let Some(file) = lookup_by_path(root, path) else {
        return;
    };
    let file_cap = file.raw_handle();
    if let Some(child) = spawn(VIEWER_ID, &[file_cap as u32]) {
        yield_now();
        revoke(file_cap);
        wait(child);
    }
}

/// Walk slash-separated `path` components from `root`, looking up each with
/// READ rights. Returns the final node's cap, or `None` on any lookup failure.
fn lookup_by_path(mut cur: Endpoint, path: &[u8]) -> Option<Endpoint> {
    let mut i = 0;
    while i <= path.len() {
        let end = path[i..]
            .iter()
            .position(|&b| b == b'/')
            .map_or(path.len(), |p| i + p);
        let component = &path[i..end];
        if !component.is_empty() {
            let req = Request::Lookup {
                name: UserBuf { ptr: component.as_ptr() as u64, len: component.len() as u64 },
                rights: FileRights::READ,
            };
            let (_words, next) = cur.call(req.encode()).ok()?;
            cur = Endpoint::from_raw_handle(next?);
        }
        i = end + 1;
    }
    Some(cur)
}

fn trim_newline(bytes: &[u8]) -> &[u8] {
    let s = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    s.strip_suffix(b"\r").unwrap_or(s)
}
