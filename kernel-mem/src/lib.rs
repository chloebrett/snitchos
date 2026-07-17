//! Memory bookkeeping (host-tested, pure): the frame-allocator bitmap
//! (`frame`), the heap watermark-grow policy (`heap`) + its smoke
//! (`heap_smoke`), and the Sv39 page-table primitives (`mmu`).
//!
//! Carved out of `kernel-core` so it builds and tests on its own — see
//! `plans/kernel-core-split.md`. Everything here is arithmetic over storage the
//! caller owns: no asm, no MMIO, no CSRs, no statics. The `kernel` binary holds
//! the live instances and does the privileged work; this crate decides what the
//! answer should be.
//!
//! Re-exported by `kernel-core` at its crate root, so the public paths stay
//! `kernel_core::frame`, `kernel_core::mmu`, etc. — moving these modules out did
//! not move them for consumers.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod frame;
pub mod heap;
pub mod heap_smoke;
pub mod mmu;
