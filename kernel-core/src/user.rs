//! Userspace / authority primitives (host-tested, pure): the per-process
//! capability table (`cap`), the ELF64 loader front-end (`elf`), the
//! synchronous endpoint rendezvous core (`ipc`), the per-process
//! userspace-defined metric table (`metric`), and the per-process span-name
//! table (`span_name`).
//!
//! Re-exported at the crate root (`pub use user::…`) so the public API stays
//! `kernel_core::cap`, `kernel_core::elf`, `kernel_core::ipc`,
//! `kernel_core::metric`, `kernel_core::span_name`.

pub mod cap;
pub mod elf;
pub mod ipc;
pub mod metric;
pub mod span_name;
