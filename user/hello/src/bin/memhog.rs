//! `workload=spawn-reap` child (spawnable id 1): reserve ~4 MiB of heap, forcing
//! the runtime allocator to `MapAnon` ~1024 user frames into our address space,
//! then exit. The `reaper` parent spawns many of us; **without** per-process
//! teardown on exit those frames leak and the kernel OOMs — this is the
//! memory-pressure body of the reclaim integration test.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;

use snitchos_user::{entry, exit_with};

#[entry]
fn main() {
    // `MapAnon` maps eagerly (no demand paging), so reserving the capacity is
    // enough to commit ~1024 frames — no need to zero or touch every byte, which
    // would burn ~1 s of CPU per child and blow the test's time budget.
    let buf = Vec::<u8>::with_capacity(4 * 1024 * 1024);
    // Read the capacity back so the reservation can't be optimized away.
    exit_with((buf.capacity() != 0) as i32);
}
