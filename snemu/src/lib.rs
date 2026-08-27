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
/// Only [`fp::RoundingMode`] is public — it appears in `cpu::StepError`, so it has to
/// be at least as visible as that. The rest of the module's items stay `pub(crate)`.
pub mod fp;
mod framebuffer;
mod fwcfg;
mod gmac;
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
