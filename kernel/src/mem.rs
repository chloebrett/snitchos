//! Memory management: the Sv39 page-table mutation primitive (`mmu`), the
//! physical frame allocator (`frame`), the kernel heap over the linear map
//! (`heap`), and its boot-time smoke (`heap_smoke`).
//!
//! Re-exported at the crate root (`pub(crate) use mem::…`) so call sites stay
//! `crate::mmu`, `crate::frame`, etc.

pub mod frame;
pub mod heap;
pub mod heap_smoke;
pub mod mmu;
