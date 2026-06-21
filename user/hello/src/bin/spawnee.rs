//! Spawned by `spawner` via the `Spawn` syscall, holding one delegated `SpanSink`
//! capability. Opens a span through that *delegated* cap (at `delegated_handle(0)`
//! = handle 2) to prove the delegation arrived and is usable — if the cap hadn't
//! been delegated, `SpanOpen` on handle 2 would be refused and no span would
//! appear. Then exits.

#![no_std]
#![no_main]

use snitchos_user::{delegated_handle, entry, exit, Tracer};

#[entry]
fn main() {
    // The parent delegated its span cap; it lands at handle 2 for the child.
    // Opening a span through it exercises the delegated authority end to end.
    let _ = Tracer::from_raw_handle(delegated_handle(0)).span("spawnee.via_delegated");
    exit();
}
