//! `workload=spawn-reap` child (spawnable id 1): reserve ~4 MiB of heap, forcing
//! the runtime allocator to `MapAnon` ~1024 user frames into our address space,
//! then exit. The `reaper` parent spawns many of us; **without** per-process
//! teardown on exit those frames leak and the kernel OOMs — this is the
//! memory-pressure body of the reclaim integration test.
//!
//! It also names *one* metric of its own before exiting, so each spawn interns a
//! per-process name. With name-GC the kernel releases it on reap — the
//! `spawn-reclaims-names` scenario watches `snitchos.intern.strings_released_total`
//! climb across the reaper's cycles.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;

use snitchos_user::{entry, exit_with, register_gauge};

#[entry]
fn main() {
    // Name a process-owned metric: each spawn interns a fresh per-process
    // `StringId` the kernel reclaims on reap (the name-GC reclaim signal).
    register_gauge("snitchos.memhog.alive").emit(1);

    // `MapAnon` maps eagerly (no demand paging), so reserving the capacity is
    // enough to commit ~1024 frames — no need to zero or touch every byte, which
    // would burn ~1 s of CPU per child and blow the test's time budget.
    let buf = Vec::<u8>::with_capacity(4 * 1024 * 1024);
    // Read the capacity back so the reservation can't be optimized away.
    exit_with((buf.capacity() != 0) as i32);
}
