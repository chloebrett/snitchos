//! Userspace / authority primitives (host-tested, pure): the per-process
//! capability table (`cap`), the ELF64 loader front-end (`elf`), and the
//! synchronous endpoint rendezvous core (`ipc`).
//!
//! Re-exported at the crate root (`pub use user::…`) so the public API stays
//! `kernel_core::cap`, `kernel_core::elf`, `kernel_core::ipc`.

pub mod cap;
pub mod elf;
pub mod ipc;
