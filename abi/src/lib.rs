//! The kernel ↔ userspace ABI.
//!
//! Shared by the kernel (syscall dispatch) and userspace programs (the
//! `ecall` site) so neither side hard-codes a magic number. `no_std`,
//! no dependencies — just the contract.
//!
//! v0.7a has exactly one syscall, invoked with **ambient authority** (any
//! U-mode code may call it, the kernel performs no capability check). v0.7b
//! reframes the same operation as a capability invocation. See
//! `docs/capability-system-design.md`.
//!
//! Calling convention (RISC-V, Linux/SBI-style): syscall number in `a7`,
//! arguments in `a0..`, result in `a0`.

#![no_std]

/// Syscall numbers, passed in register `a7` at the `ecall`.
///
/// Postcard-free, plain integers — this is a register ABI, not a wire
/// format. New syscalls append; never renumber an existing one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub enum Syscall {
    /// Emit a telemetry metric on the caller's behalf. Argument `a0` is
    /// the value. Ambient in v0.7a; gated by a `TelemetrySink` capability
    /// in v0.7b.
    EmitMetric = 0,
}

impl Syscall {
    /// Resolve a raw `a7` value to a known syscall, or `None` if the
    /// number names nothing. The kernel uses this to reject unknown
    /// syscalls rather than trusting the register blindly.
    #[must_use]
    pub const fn from_usize(n: usize) -> Option<Self> {
        match n {
            0 => Some(Self::EmitMetric),
            _ => None,
        }
    }
}
