//! snemu — the `SnitchOS` emulator.
//!
//! A small RV64GC interpreter. See `docs/snemu-design.md` for scope and
//! `plans/snemu-milestone-1-console-out.md` for the current milestone.

mod bus;
pub mod cpu;
mod csr;
mod decode;
pub mod loader;
mod mmu;
pub mod mem;
mod uart;
