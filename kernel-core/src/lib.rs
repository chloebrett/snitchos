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
pub mod console;
pub mod framebuffer;
pub mod fwcfg;
pub mod notify;
pub mod ramfb;
pub mod reap;
pub mod sched;
pub mod stack;
pub mod trap;
pub mod virtio;

mod user;
mod workloads;

// `mem` and `obs` now live in their own crates so they build and test without the
// rest of kernel-core — see `plans/kernel-core-split.md`. Re-exported here so the
// public paths stay `kernel_core::mmu`, `kernel_core::intern`, etc.: the moves are
// invisible to consumers. These re-exports are scaffolding — the plan's final step
// removes them, repoints callers at the real crates, and deletes this crate.
pub use kernel_mem::{frame, heap, heap_smoke, mmu};
pub use kernel_obs::{batch_ring, clock, intern, panic_log, preinit, sink, span};
pub use user::{cap, elf, ipc, metric, span_name};
pub use workloads::{bootargs, workload};
