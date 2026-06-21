//! `workload=spawn-reap` parent: spawn + wait a memory-hungry child many times.
//!
//! Each `memhog` child (spawnable id 1) allocates ~4 MiB and exits; the parent
//! reaps it with `wait`. With per-process teardown on Exit those frames are
//! reclaimed (`snitchos.frames.freed_total` climbs into the thousands and the
//! kernel never OOMs); **without** it they leak — ~120 MiB > RAM — and the kernel
//! OOM-panics before the loop finishes. Emits `reaper.done` once every child has
//! been reaped, the GREEN-only completion marker.

#![no_std]
#![no_main]

use snitchos_user::{entry, exit, spawn, tracer, wait};

#[entry]
fn main() {
    let mut n = 0;
    while n < 30 {
        // Program id 1 = `memhog`. No caps delegated — it only needs ambient
        // `MapAnon` + `Exit`.
        if let Some(child) = spawn(1, &[]) {
            let _ = wait(child);
        }
        n += 1;
    }
    let _ = tracer().span("reaper.done");
    exit();
}
