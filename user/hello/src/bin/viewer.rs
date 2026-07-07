//! `view-demo` child: receives a scoped READ-only file cap at `"file"`,
//! reads it, prints the bytes to the UART console, and emits
//! `snitchos.viewer.bytes_read` with the total byte count.

#![no_std]
#![no_main]

use fs_proto::{Op, Request, Response, UserBuf};
use snitchos_user::{Endpoint, bootstrap, console_write, entry, register_counter, yield_now};

const BUF_SIZE: usize = 512;

#[entry(needs = [("file", ENDPOINT, SEND)])]
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
    // Hold the cap alive while the parent revokes it. A cap released on
    // process exit vanishes silently (no CapEvent::Revoked); revoke only
    // fires when the holder is still running. Four yields give the parent
    // enough scheduling turns to call revoke before we exit.
    //
    // TODO: replace with a proper parent-signals-done primitive once one
    // exists. The yield count is fragile: it assumes the parent wins the
    // CPU within four scheduling turns, which holds today under cooperative
    // round-robin but is an implicit timing contract, not a hard guarantee.
    for _ in 0..4 {
        yield_now();
    }
}
