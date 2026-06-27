//! `workload=init` — the supervising root (v0.13 Step 4), the shape the real
//! first userspace process will take. Holds only its bootstrap telemetry/span
//! (the kernel delegates it nothing else). It `Spawn`s a child — delegating its
//! own span cap downward — then **supervises**: `wait_any()` reaps whichever
//! child exits, reporting the reaped id + status. After the child is reaped the
//! loop's next `wait_any()` blocks (no more children), the idle-supervisor shape.
//!
//! This is `init` proving it can be the root of the delegation graph: spawn with
//! delegated authority, then reap. Steps 5–7 grow it to manufacture an endpoint
//! and bring up the FS server + a client; Step 8 makes it the default boot.

#![no_std]
#![no_main]

use snitchos_user::{endpoint_create, entry, register_counter, span_handle, spawn, wait_any};

#[entry]
fn main() {
    // Program id 0 = `spawnee` (exits 42, opening a span through a delegated cap).
    // Hand it our span cap — the delegation is a visible `CapEvent::Transferred`
    // rooted at init's holding.
    let _ = spawn(0, &[span_handle()]);

    // Manufacture our own IPC endpoint and bring up the FS server on it (Step 6).
    // The kernel handed init no endpoint — it builds its own IPC world. Delegate
    // the owning `RECV | MINT` cap to the server (program id 4 = `fs-server`); the
    // grant is a `CapEvent::Transferred` rooted at init's endpoint holding.
    let fs_endpoint = endpoint_create();
    let _ = spawn(4, &[fs_endpoint.raw_handle() as u32]);

    // Supervise: reap whichever child exits, reporting its id + status. We never
    // named the child — `wait_any` is the supervising-parent primitive.
    loop {
        let (status, child) = wait_any();
        register_counter("snitchos.init.reaped_status").emit(i64::from(status));
        register_counter("snitchos.init.reaped_child").emit(i64::from(child));
    }
}
