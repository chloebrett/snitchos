//! Host-testable kernel logic. Pure data structures, no asm, no MMIO,
//! no CSRs — anything in here must compile and run on the host so we
//! can unit-test it with `cargo test -p kernel-core`.
//!
//! See `plans/kernel-core-carveout.md` for what lives here vs. what
//! stays in the `kernel` binary.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod clock;
pub mod heap_smoke;
pub mod frame;
pub mod heap;
pub mod intern;
pub mod mmu;
pub mod preinit;
pub mod sched;
pub mod sink;
pub mod span;
pub mod trap;
