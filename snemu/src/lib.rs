//! snemu — the `SnitchOS` emulator.
//!
//! A small RV64GC interpreter. See `docs/snemu-design.md` for scope and
//! `plans/legacy/snemu-milestone-1-console-out.md` for the current milestone.

pub mod audio;
mod block;
mod bus;
pub mod bench;
pub mod cpu;
pub mod dtb;
mod csr;
mod decode;
mod fetch_cache;
mod framebuffer;
mod fwcfg;
mod jit;
pub mod loader;
pub mod machine;
mod mmu;
pub mod mem;
mod plic;
mod pwmdac;
pub mod symbols;
mod uart;
mod virtio;
