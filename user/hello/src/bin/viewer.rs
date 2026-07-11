//! `view-demo`/`shell` child: receives a scoped READ-only file cap at `"file"`,
//! reads it, prints the bytes to the UART, and emits `snitchos.viewer.bytes_read`.
//! Then runs the **readiness/done handshake** (supervision-design D2), replacing the
//! old fragile yield-count timing bet. It `Signal`s `"done"` — telling the parent the
//! read is complete, so the parent revokes only now (grant → use → reclaim: `bytes_read`
//! before `Revoked` on the wire) — then `WaitNotify`s `"proceed"`, staying alive while
//! the parent revokes so `CapEvent::Revoked` fires with a live holder (a cap released on
//! process exit fires none). The parent signals `"proceed"` after revoking; the viewer
//! wakes and exits.

#![no_std]
#![no_main]

use fs_proto::{Op, Request, Response, UserBuf};
use snitchos_user::{Endpoint, Notification, bootstrap, console_write, entry, register_counter};

const BUF_SIZE: usize = 512;

#[entry(needs = [
    ("file", ENDPOINT, SEND),
    ("done", NOTIFICATION, SIGNAL),
    ("proceed", NOTIFICATION, WAIT),
])]
fn main() {
    let bytes_read: snitchos_user::Metric = register_counter("snitchos.viewer.bytes_read");

    let Some(file) = bootstrap().get::<Endpoint>("file") else {
        return;
    };

    let mut buf = [0u8; BUF_SIZE];
    let mut total: u64 = 0;
    let mut offset: u64 = 0;

    loop {
        let read = Request::Read {
            offset,
            dst: UserBuf {
                ptr: buf.as_mut_ptr() as u64,
                len: buf.len() as u64,
            },
        };
        let Ok((words, _)) = file.call(read.encode()) else {
            break;
        };
        let n = match Response::decode(Op::Read, words) {
            Ok(Response::Count(n)) => n as usize,
            _ => break,
        };
        if n == 0 {
            break;
        }
        console_write(&buf[..n]);
        total += n as u64;
        offset += n as u64;
        if n < BUF_SIZE {
            break;
        }
    }

    bytes_read.emit(total as i64);

    // The read is complete — tell the parent so it revokes only now. `signal`/`wait`
    // coalesce, so this is order-independent (no lost wakeup if the parent hasn't
    // reached its `wait`/`signal` yet).
    if let Some(done) = bootstrap().get::<Notification>("done") {
        let _ = done.signal(1);
    }
    // Block until the parent has revoked and released us — keeps us alive across the
    // revoke so `CapEvent::Revoked` fires with a live holder.
    if let Some(proceed) = bootstrap().get::<Notification>("proceed") {
        let _ = proceed.wait();
    }
}
