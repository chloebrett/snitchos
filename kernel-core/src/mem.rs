//! Memory bookkeeping (host-tested, pure): the frame-allocator bitmap
//! (`frame`), the heap watermark-grow policy (`heap`) + its smoke
//! (`heap_smoke`), and the Sv39 page-table primitives (`mmu`).
//!
//! Re-exported at the crate root (`pub use mem::…`) so the public API stays
//! `kernel_core::frame`, `kernel_core::mmu`, etc.

pub mod frame;
pub mod heap;
pub mod heap_smoke;
pub mod mmu;
