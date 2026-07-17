//! Memory bookkeeping (host-tested, pure): the frame-allocator bitmap
//! (`frame`), the heap watermark-grow policy (`heap`) + its smoke
//! (`heap_smoke`), and the Sv39 page-table primitives (`mmu`).
//!
//! Carved out of `kernel-core` so it builds and tests on its own — see
//! `plans/legacy/kernel-core-split.md`. Everything here is arithmetic over
//! storage the caller owns: no asm, no MMIO, no CSRs, no statics. The `kernel`
//! binary holds the live instances and does the privileged work; this crate
//! decides what the answer should be.
//!
//! The only crate any other `kernel-*` crate depends on: `kernel-proc`'s `stack`
//! needs `mmu::PAGE_SIZE`. Otherwise this is a leaf — no dependencies at all.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod frame;
pub mod heap;
pub mod heap_smoke;
pub mod mmu;
