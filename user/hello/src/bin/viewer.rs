//! `view-demo` child: receives a scoped READ-only file cap at `"file"`,
//! reads it, prints the bytes to the UART console, and emits
//! `snitchos.viewer.bytes_read` with the total byte count.

#![no_std]
#![no_main]

use fs_proto::{Op, Request, Response, UserBuf};
use snitchos_user::{Endpoint, bootstrap, console_write, entry, register_counter};

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
}
