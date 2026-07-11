//! Host-testable kernel logic. Pure data structures, no asm, no MMIO,
//! no CSRs — anything in here must compile and run on the host so we
//! can unit-test it with `cargo test -p kernel-core`.
//!
//! See `plans/kernel-core-carveout.md` for what lives here vs. what
//! stays in the `kernel` binary.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

// Modules are grouped by concern into directory modules (`mem/`, `obs/`,
// `user/`, `workloads/`). Each group re-exports its children at the crate root
// via `pub use` below, so the public API stays `kernel_core::frame`,
// `kernel_core::cap`, etc. — the physical nesting doesn't change the paths.
// Single-module concerns (`clock`, `sched`, `trap`, `virtio`) stay at the root.
pub mod clock;
pub mod console;
pub mod notify;
pub mod panic_log;
pub mod reap;
pub mod sched;
pub mod stack;
pub mod trap;
pub mod virtio;

mod mem;
mod obs;
mod user;
mod workloads;

pub use mem::{frame, heap, heap_smoke, mmu};
pub use obs::{batch_ring, intern, preinit, sink, span};
pub use user::{cap, elf, ipc, metric, span_name};
pub use workloads::{bootargs, workload};
