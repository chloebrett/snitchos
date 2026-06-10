//! The kernel ↔ userspace ABI.
//!
//! Shared by the kernel (syscall dispatch) and userspace programs (the
//! `ecall` site) so neither side hard-codes a magic number. `no_std`,
//! no dependencies — just the contract.
//!
//! v0.7b: the kernel surface is **invoke a capability**. A program names a
//! capability by an opaque handle (an index into *its own* `CapTable`) and
//! the kernel validates every invocation against that table — no ambient
//! authority. (v0.7a's `EmitMetric` was the deliberately-wrong ambient
//! version this replaces.) See `docs/capability-system-design.md`.
//!
//! Calling convention (RISC-V, Linux/SBI-style): syscall number in `a7`,
//! arguments in `a0..`, result in `a0`. For `Invoke`: `a0` = capability
//! handle, `a1` = the operation's argument; `a0` on return is `0` on
//! success or nonzero on a denied/unknown invocation.

#![no_std]

/// Syscall numbers, passed in register `a7` at the `ecall`.
///
/// Postcard-free, plain integers — this is a register ABI, not a wire
/// format. New syscalls append; never renumber an existing one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub enum Syscall {
    /// Invoke a capability. `a0` = handle (into the caller's `CapTable`),
    /// `a1` = argument. The kernel resolves the handle against the caller's
    /// table, checks the capability's rights, and performs the authorized
    /// operation — for a `TelemetrySink`, emitting `a1` to the bound
    /// counter. The single kernel surface; "syscalls" are messages to
    /// capabilities.
    Invoke = 0,
    /// Terminate the calling process. Does not return — the kernel marks
    /// the user task `Exited` and switches the hart to its next ready task.
    /// (Not capability-mediated: a process can always end itself. v0.7b
    /// leaks the address space + caps on exit; reclamation is later.)
    Exit = 1,
}

impl Syscall {
    /// Resolve a raw `a7` value to a known syscall, or `None` if the
    /// number names nothing. The kernel uses this to reject unknown
    /// syscalls rather than trusting the register blindly.
    #[must_use]
    pub const fn from_usize(n: usize) -> Option<Self> {
        match n {
            0 => Some(Self::Invoke),
            1 => Some(Self::Exit),
            _ => None,
        }
    }
}

/// The handle the bootstrap `TelemetrySink` capability lands at in a
/// freshly granted process — the well-known root cap `init` is born
/// holding. Matches the first slot of an empty `CapTable` (`index 0`,
/// `generation 0`), so its raw value is `0`. The kernel grants it; the
/// program invokes it.
pub const TELEMETRY_SINK_HANDLE: usize = 0;
