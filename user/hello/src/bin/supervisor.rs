//! `workload=wait-any` parent: a supervising process that spawns two children —
//! a never-exiting `spinner` and an exiting `spawnee` — then `wait_any()`s for
//! whichever exits. Proves the supervising parent (the `init` shape) wakes on the
//! exiting child *without naming it*, collecting its id + status, while the
//! long-lived sibling keeps running. Emits the reaped id + status as telemetry.

#![no_std]
#![no_main]

use snitchos_user::{entry, exit, register_counter, span_handle, spawn, wait_any};

#[entry]
fn main() {
    // Program id 3 = `spinner` (never exits); id 0 = `spawnee` (exits 42, opening
    // a span through a delegated cap — hand it our span cap like `spawner` does).
    let _ = spawn(3, &[]);
    let _ = spawn(0, &[span_handle()]);

    // Wait for *any* child. The spinner never exits, so this returns the spawnee
    // — its status (42) and task id — without us having named a child.
    let (status, child) = wait_any();
    register_counter("snitchos.supervisor.any_status").emit(i64::from(status));
    register_counter("snitchos.supervisor.any_child").emit(i64::from(child));
    exit();
}
