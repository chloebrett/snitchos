//! snemu — the `SnitchOS` emulator.
//!
//! A small RV64GC interpreter. See `docs/snemu-design.md` for scope and
//! `plans/snemu-milestone-1-console-out.md` for the current milestone.

mod block;
mod bus;
pub mod bench;
pub mod cpu;
pub mod dtb;
mod csr;
mod decode;
mod decode_cache;
mod jit;
pub mod loader;
pub mod machine;
mod mmu;
pub mod mem;
pub mod symbols;
mod uart;
mod virtio;
