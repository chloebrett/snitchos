//! `workload=spawn-demo` — the spawn-with-caps demo parent.
//!
//! Delegates its own `SpanSink` capability to a freshly-`spawn`ed `spawnee`
//! child (program id 0 in the kernel's spawnable registry), proving the `Spawn`
//! syscall carries authority *downward*. Emits `spawner.spawned` once the child
//! is launched, then exits.

#![no_std]
#![no_main]

use snitchos_user::{entry, exit, register_counter, spawn, span_handle, tracer, wait};

#[entry]
fn main() {
    // Hand the child exactly our span cap — nothing else. Program id 0 = `spawnee`.
    let Some(child) = spawn(0, &[span_handle()]) else {
        let _ = tracer().span("spawner.refused");
        exit();
    };
    let _ = tracer().span("spawner.spawned");

    // Reap the child and report the status we collected — proves Wait round-trips
    // the child's exit code (v0.12). The child exits with 42.
    let status = wait(child);
    register_counter("snitchos.spawner.marker").emit(status as i64);
    exit();
}
