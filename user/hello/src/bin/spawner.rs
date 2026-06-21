//! `workload=spawn-demo` — the spawn-with-caps demo parent.
//!
//! Delegates its own `SpanSink` capability to a freshly-`spawn`ed `spawnee`
//! child (program id 0 in the kernel's spawnable registry), proving the `Spawn`
//! syscall carries authority *downward*. Emits `spawner.spawned` once the child
//! is launched, then exits.

#![no_std]
#![no_main]

use snitchos_user::{entry, exit, spawn, span_handle, tracer};

#[entry]
fn main() {
    // Hand the child exactly our span cap — nothing else. Program id 0 = `spawnee`.
    let child = spawn(0, &[span_handle()]);
    let _ = tracer().span(if child.is_some() {
        "spawner.spawned"
    } else {
        "spawner.refused"
    });
    exit();
}
