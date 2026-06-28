//! Demonstrates `#[entry]`'s manifest clause: the typed `~>`-stage interface is
//! const-encoded into a `.note.snitch.iface` ELF note (the seed step lifts that
//! into the program's `user.iface` xattr). This stage reads a `Row`, produces a
//! `u64`, and declares it uses `ConsoleOut`. It just yields forever — the point
//! is the section, not the behavior.

#![no_std]
#![no_main]

use hitch::Schema;
use snitchos_user::{entry, yield_now};

#[derive(Schema)]
#[allow(dead_code, reason = "reflected into the manifest via SCHEMA, not read")]
struct Row {
    name: u32,
    count: i64,
}

#[entry(in = Row, out = u64, uses = [ConsoleOut])]
fn main() {
    loop {
        yield_now();
    }
}
