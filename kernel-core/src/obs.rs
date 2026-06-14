//! Observability primitives (host-tested, pure): the string intern table
//! (`intern`), the span registry + cursor (`span`), the `FrameSink` abstraction
//! (`sink`), the pre-virtio frame buffer (`preinit`), and the cross-hart batched
//! SPSC ring (`batch_ring`).
//!
//! Re-exported at the crate root (`pub use obs::…`) so the public API stays
//! `kernel_core::span`, `kernel_core::sink`, etc.

pub mod batch_ring;
pub mod intern;
pub mod preinit;
pub mod sink;
pub mod span;
