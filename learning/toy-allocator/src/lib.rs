//! toy-allocator — a standalone playground for the two allocation
//! strategies the kernel uses:
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
//! Both ship with failing tests and `todo!()` exercises. See
//! `EXERCISES.md`. Run `cargo test -p toy-allocator`.

pub mod bitmap;
pub mod freelist;
