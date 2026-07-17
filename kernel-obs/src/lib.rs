//! How the kernel talks about itself: the intern table (`intern`), span
//! bookkeeping (`span`), the pre-MMU frame buffer (`preinit`), the emit seam
//! (`sink`), the batching ring (`batch_ring`), the alloc-free panic encoder
//! (`panic_log`), and the `Clock` trait (`clock`).
//!
//! Carved out of `kernel-core` — see `plans/kernel-core-split.md`. Pure
//! bookkeeping: no asm, no MMIO, no statics. The `kernel` binary owns the live
//! instances and the actual wire; this crate decides *what* to say and *when*,
//! never *how* it gets out. That seam is [`sink::FrameSink`].
//!
//! This is the only `kernel-*` crate that depends on `protocol` — observability
//! is the one concern here that has a wire format at all.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod batch_ring;
pub mod clock;
pub mod intern;
pub mod panic_log;
pub mod preinit;
pub mod sink;
pub mod span;
