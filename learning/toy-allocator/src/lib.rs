//! toy-allocator — a standalone playground for the three allocation
//! strategies worth knowing:
//!
//! - [`freelist`] — a free-list allocator with **splitting** and
//!   **coalescing**. This is the model behind the kernel *heap*
//!   (`vendor/linked_list_allocator`, driven by `kernel/src/heap.rs`).
//!   Variable-size allocations; fragmentation is a real concern.
//!
//! - [`bitmap`] — a one-bit-per-frame allocator. This is the model
//!   behind the *physical frame allocator*
//!   (`kernel-core/src/frame.rs`). Fixed-size allocations; no
//!   fragmentation, but no variable sizes either.
//!
//! - [`buddy`] — a power-of-two buddy allocator. Not used by SnitchOS
//!   (which chose the bitmap), but it's what *Linux* uses for physical
//!   pages. O(1) coalescing via the XOR-buddy trick; the tradeoff is
//!   internal fragmentation (sizes round up to a power of two).
//!
//! All ship with failing tests and `todo!()` exercises. See
//! `EXERCISES.md`. Run `cargo test -p toy-allocator`.

pub mod bitmap;
pub mod buddy;
pub mod freelist;
