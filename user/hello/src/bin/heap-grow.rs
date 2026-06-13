//! Heap-growth probe (`workload=heap-grow`): allocate far past the runtime's
//! per-region `map_anon` size, forcing the `talc` allocator to request more
//! frames from the kernel on demand. Fills the buffer and sums it back so every
//! page is actually written and read (proving the mapped frames are real R/W
//! memory, not just reserved address space), then emits the sum as a marker —
//! which only appears if the whole allocation succeeded (a failed grow would
//! alloc-error → panic → no marker).

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec;

use snitchos_user::{entry, telemetry};

/// 512 KiB — well past the 64 KiB minimum map, so the heap must grow.
const SIZE: usize = 512 * 1024;

#[entry]
fn main() {
    // `vec![1u8; SIZE]` heap-allocates SIZE bytes (forcing growth) and writes
    // each one. Summing reads them all back.
    let buf = vec![1u8; SIZE];
    let sum: i64 = buf.iter().map(|&b| i64::from(b)).sum();
    // sum == SIZE (524288) iff every byte was allocated, written, and readable.
    let _ = telemetry().emit(sum);
}
