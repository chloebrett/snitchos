//! **Scaffolding — this crate is on its way out.**
//!
//! `kernel-core` was a staging area: a grab-bag whose only meaning was "the
//! host-testable bits". That stopped being a distinction once every part of it
//! became a host-testable crate in its own right. It now holds **no code** — only
//! the re-exports below, which keep `kernel_core::…` paths resolving while the
//! split lands one crate at a time.
//!
//! The final step of `plans/kernel-core-split.md` deletes these re-exports,
//! repoints callers at the five real crates, and removes this crate. Don't add
//! anything here; add it to whichever crate owns the concept:
//!
//! - [`kernel_mem`] — memory bookkeeping (mmu, frame, heap)
//! - [`kernel_obs`] — how the kernel talks about itself (intern, span, sink)
//! - [`kernel_devices`] — device protocol logic (virtio, fwcfg, ramfb)
//! - [`kernel_boot`] — boot-time decisions (bootargs, workload, trap)
//! - [`kernel_proc`] — tasks, authority, lifecycle (sched, cap, ipc, elf)

#![no_std]
#![forbid(unsafe_code)]

pub use kernel_boot::{bootargs, trap, workload};
pub use kernel_devices::{console, framebuffer, fwcfg, ramfb, virtio};
pub use kernel_mem::{frame, heap, heap_smoke, mmu};
pub use kernel_obs::{batch_ring, clock, intern, panic_log, preinit, sink, span};
pub use kernel_proc::{cap, elf, ipc, metric, notify, reap, sched, span_name, stack};
