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
    /// Map a fresh anonymous memory region. `a0` = bytes requested
    /// (page-aligned by the runtime). The kernel maps that many bytes of fresh
    /// zeroed frames into the process's address space and returns the region's
    /// **base** VA in `a0`, or `usize::MAX` if refused (out of frames, or past
    /// the per-process memory cap). mmap-shaped, not `brk`: a region is
    /// returned (individually unmappable later, and a `MemoryRegion` capability
    /// eventually), and the runtime allocator (`talc`) `claim`s each one — it
    /// does not assume regions abut, so the kernel may place them disjointly.
    MapAnon = 5,
    /// Write bytes to the debug/stdout channel. `a0` = pointer to the bytes,
    /// `a1` = length. The kernel copies them out and emits a snitched `Log`
    /// wire frame (so stdout is observable). Returns bytes written in `a0`, or
    /// `usize::MAX` if refused (bad pointer). Backs `println!`.
    DebugWrite = 6,
    /// Send an inline message over a synchronous IPC endpoint (v0.9). `a0` =
    /// `Endpoint` capability handle (needs `SEND`), `a1..=a4` = the four
    /// message words. Rendezvous semantics: if a receiver is waiting the
    /// message is delivered and both proceed; otherwise the sender blocks until
    /// one arrives. Returns `0` in `a0` on success, `usize::MAX` if refused
    /// (bad/again wrong-rights/wrong-object handle).
    Send = 7,
    /// Receive an inline message from a synchronous IPC endpoint (v0.9). `a0` =
    /// `Endpoint` capability handle (needs `RECV`). Blocks until a sender
    /// rendezvouses; returns `0` in `a0` and the four message words in
    /// `a1..=a4`, or `usize::MAX` in `a0` if refused. For an RPC `call`, the
    /// reply-cap handle is returned in `a5` (`0` for a one-way `send`).
    Receive = 8,
    /// RPC `call` over a synchronous endpoint (v0.9b): send a request **and**
    /// block for a reply. `a0`=`Endpoint` handle (needs `SEND`), `a1..=a4`=
    /// request words. The kernel mints a one-shot reply cap into the receiver
    /// at the rendezvous; the caller parks until `reply`. Returns `0` in `a0`
    /// and the reply words in `a1..=a4` (or `usize::MAX` if refused).
    Call = 9,
    /// Answer an RPC (v0.9b). `a0`=reply-cap handle (from `receive`'s `a5`),
    /// `a1..=a4`=response words. Wakes the blocked caller and **consumes** the
    /// one-shot reply cap (a second `reply` is refused). Returns `0`, or
    /// `usize::MAX` if the handle is not a live reply cap.
    Reply = 10,
    /// Fused `reply`-then-`receive` (v0.9b) — the server hot path. `a0` =
    /// `Endpoint` handle (needs `RECV`), `a5` = the previous request's reply
    /// handle (`0` on the first iteration — no prior reply), `a1..=a4` = the
    /// response to that previous request. Replies the previous caller (if any),
    /// then blocks receiving the next request: returns `0` in `a0`, the next
    /// request words in `a1..=a4`, and its reply handle in `a5`. One trap
    /// instead of two per request.
    ReplyRecv = 11,
    /// Mint a badged `SEND` capability for an endpoint the caller owns (v0.9c).
    /// `a0` = endpoint handle (needs `MINT`), `a1` = the server-chosen `badge`
    /// (u64), `a2` = the requested rights bits. The kernel derives a child cap
    /// naming the same endpoint, stamped with the badge + rights, and inserts it
    /// into the caller's own table. Returns the new handle in `a0`, or
    /// `usize::MAX` if refused (handle lacks `MINT` / names no endpoint). The
    /// minted cap is handed to a client via cap-transfer (a later step).
    MintBadged = 12,
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
            5 => Some(Self::MapAnon),
            6 => Some(Self::DebugWrite),
            7 => Some(Self::Send),
            8 => Some(Self::Receive),
            9 => Some(Self::Call),
            10 => Some(Self::Reply),
            11 => Some(Self::ReplyRecv),
            12 => Some(Self::MintBadged),
            _ => None,
        }
    }
}

/// Capability rights bits — the bitmask carried on a capability and on the
/// `CapEvent` wire frame, and the rights a [`Syscall::MintBadged`] requests.
/// The single source of truth: the kernel's typed `kernel_core::cap::Rights`
/// wraps these, and userspace passes them raw. Neither side hard-codes the
/// values. Binary literals (next bit `0b1_0000`) — no `1 << n` to misread.
pub mod rights {
    /// May emit telemetry through a `TelemetrySink`.
    pub const EMIT: u32 = 0b0001;
    /// May `send` on an `Endpoint`.
    pub const SEND: u32 = 0b0010;
    /// May `receive` on an `Endpoint`.
    pub const RECV: u32 = 0b0100;
    /// May mint badged `SEND` caps for an `Endpoint` the holder owns (v0.9c).
    pub const MINT: u32 = 0b1000;
}

/// The number of inline `u64` words a single IPC message carries. The single
/// source of truth shared by the kernel, the userspace runtime, and any wire
/// protocol layered on IPC (e.g. `fs-proto`). Larger payloads cross via a
/// copy/`MemoryRegion` mechanism, not by widening this.
pub const MSG_WORDS: usize = 4;

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
        assert_eq!(Syscall::MapAnon as usize, 5);
        assert_eq!(Syscall::DebugWrite as usize, 6);
        assert_eq!(Syscall::Send as usize, 7);
        assert_eq!(Syscall::Receive as usize, 8);
        assert_eq!(Syscall::Call as usize, 9);
        assert_eq!(Syscall::Reply as usize, 10);
        assert_eq!(Syscall::ReplyRecv as usize, 11);
        assert_eq!(Syscall::MintBadged as usize, 12);

        assert_eq!(Syscall::from_usize(0), Some(Syscall::Invoke));
        assert_eq!(Syscall::from_usize(1), Some(Syscall::Exit));
        assert_eq!(Syscall::from_usize(2), Some(Syscall::Yield));
        assert_eq!(Syscall::from_usize(3), Some(Syscall::SpanOpen));
        assert_eq!(Syscall::from_usize(4), Some(Syscall::SpanClose));
        assert_eq!(Syscall::from_usize(5), Some(Syscall::MapAnon));
        assert_eq!(Syscall::from_usize(6), Some(Syscall::DebugWrite));
        assert_eq!(Syscall::from_usize(7), Some(Syscall::Send));
        assert_eq!(Syscall::from_usize(8), Some(Syscall::Receive));
        assert_eq!(Syscall::from_usize(9), Some(Syscall::Call));
        assert_eq!(Syscall::from_usize(10), Some(Syscall::Reply));
        assert_eq!(Syscall::from_usize(11), Some(Syscall::ReplyRecv));
        assert_eq!(Syscall::from_usize(12), Some(Syscall::MintBadged));
        assert_eq!(Syscall::from_usize(13), None);
    }
}
