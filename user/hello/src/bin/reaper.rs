//! `workload=spawn-reap` parent: spawn + wait a memory-hungry child many times.
//!
//! Each `memhog` child (spawnable id 1) allocates ~4 MiB and exits; the parent
//! reaps it with `wait`. With per-process teardown on Exit those frames are
//! reclaimed (`snitchos.frames.freed_total` climbs into the thousands and the
//! kernel never OOMs); **without** it they leak and the kernel OOM-panics before
//! the loop finishes. Emits `reaper.done` once every child has been reaped, the
//! GREEN-only completion marker.
//!
//! `CHILDREN` × 4 MiB must comfortably **exceed the machine RAM** so the leak case
//! reliably OOMs — the teeth, not a coincidence of kernel overhead. The suite runs
//! this workload on a small (48 MiB) machine (`snemu_diff::ram_mb_for`), so 15
//! children = 60 MiB > 48 MiB clears it with margin while keeping the child count
//! (and thus the zero-a-frame `memset` cost that dominates this scenario) low. The
//! `spawn-reclaims-names` itest asserts a released-name count tied to `CHILDREN` —
//! keep them in sync.

#![no_std]
#![no_main]

use snitchos_user::{entry, exit, spawn, tracer, wait};

/// Spawn/reap cycles. `CHILDREN` × 4 MiB must exceed the (48 MiB) machine RAM so a
/// non-reclaiming kernel OOMs — see the module doc. Kept in sync with the
/// `spawn-reclaims-names` itest threshold.
const CHILDREN: u32 = 15;

#[entry]
fn main() {
    let mut n = 0;
    while n < CHILDREN {
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
