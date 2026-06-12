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
    /// Voluntarily yield the CPU. The kernel runs `yield_now()` on the
    /// caller's behalf — switching to the next ready task — then returns
    /// here on a later reschedule, so control resumes after the `ecall`.
    /// Not capability-mediated: yielding grants no authority, it only
    /// relinquishes the CPU. The cooperative path; preemption (v0.8) is
    /// the involuntary counterpart.
    Yield = 2,
    /// Open a span. `a0` = `SpanSink` capability handle, `a1` = pointer to
    /// the span name in user memory, `a2` = its length. The kernel copies
    /// and interns the name, opens a span on the caller's task cursor, and
    /// returns an opaque span id in `a0` (or an error sentinel if refused).
    SpanOpen = 3,
    /// Close a span previously opened with [`Self::SpanOpen`]. `a0` = the
    /// span id the open returned. Emits the matching `SpanEnd`.
    SpanClose = 4,
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
            2 => Some(Self::Yield),
            3 => Some(Self::SpanOpen),
            4 => Some(Self::SpanClose),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn syscall_numbers_round_trip() {
        assert_eq!(Syscall::Invoke as usize, 0);
        assert_eq!(Syscall::Exit as usize, 1);
        assert_eq!(Syscall::Yield as usize, 2);
        assert_eq!(Syscall::SpanOpen as usize, 3);
        assert_eq!(Syscall::SpanClose as usize, 4);

        assert_eq!(Syscall::from_usize(0), Some(Syscall::Invoke));
        assert_eq!(Syscall::from_usize(1), Some(Syscall::Exit));
        assert_eq!(Syscall::from_usize(2), Some(Syscall::Yield));
        assert_eq!(Syscall::from_usize(3), Some(Syscall::SpanOpen));
        assert_eq!(Syscall::from_usize(4), Some(Syscall::SpanClose));
        assert_eq!(Syscall::from_usize(5), None);
    }
}

