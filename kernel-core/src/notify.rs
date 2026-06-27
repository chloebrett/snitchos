//! The **notification** primitive — the general async kernel→user signal.
//!
//! A [`Notification`] is one machine word of pending-signal bits plus at most
//! one parked waiter. [`signal`](Notification::signal) OR-s a mask into the
//! pending word and wakes any waiter; it never blocks. [`wait`](Notification::wait)
//! returns-and-clears the pending word if it is nonzero, otherwise parks the
//! caller. Coalescing falls straight out — N signals before anyone waits collapse
//! into one wake carrying the OR of their masks — so there is **no queue and no
//! per-signal kernel memory**. A notification answers *"did it happen?"*, never
//! *"how many times, in what order, with what payload"*; that is an
//! [`crate::ipc`] endpoint's job.
//!
//! Pure data + bookkeeping, host-tested here exactly like [`crate::reap`] and
//! [`crate::ipc`]: no `unsafe`, no MMIO, no CSRs. The kernel owns the live table
//! (behind a `Mutex`) and does the `block_current`/`wake` wiring; this core only
//! says *what to do* via [`SignalStep`] / [`WaitStep`].
//!
//! The bit *mask* is the one word of meaning permitted, and it is
//! **userspace-defined** — like an endpoint badge, the kernel never reads it
//! beyond OR-ing and delivering it. One waiter per notification in v0.12: a
//! second waiter is *refused* ([`WaitStep::Busy`]), not silently dropped — the
//! lesson from [`crate::reap`]'s single-slot waiter overwrite. Multi-waiter
//! fan-out is a documented growth point.
//!
//! See [docs/notification-design.md](../../../docs/notification-design.md) and
//! `plans/v0.12-notifications.md`.

use crate::sched::TaskId;

/// What a [`Notification::signal`] caller must do next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalStep {
    /// A waiter was parked on this notification — the kernel must `wake` it.
    /// The pending bits are left set for the woken task's re-[`wait`](Notification::wait).
    Woke(TaskId),
    /// Nobody was parked; the bits are now pending for whoever waits next.
    NoWaiter,
}

/// What a [`Notification::wait`] caller must do next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitStep {
    /// Bits were already pending — they have been taken (cleared); return them.
    Ready(u64),
    /// No bits pending — the caller has been recorded as the waiter and must block.
    Block,
    /// Another task is already parked on this notification — refuse this waiter
    /// (the kernel snitches a `SyscallRefused`). One waiter per notification.
    Busy,
}

/// One machine word of pending-signal bits plus at most one parked waiter.
#[derive(Debug, Default)]
pub struct Notification {
    pending: u64,
    waiter: Option<TaskId>,
}

impl Notification {
    /// An empty notification — no pending bits, no waiter.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            pending: 0,
            waiter: None,
        }
    }

    /// OR `mask` into the pending bits. If a waiter is parked, it must be woken
    /// ([`SignalStep::Woke`]); otherwise the bits wait for the next
    /// [`wait`](Self::wait) ([`SignalStep::NoWaiter`]). Never clears the waiter
    /// here — the woken task re-runs `wait` and takes the bits itself.
    pub fn signal(&mut self, mask: u64) -> SignalStep {
        self.pending |= mask;
        match self.waiter {
            Some(id) => SignalStep::Woke(id),
            None => SignalStep::NoWaiter,
        }
    }

    /// If bits are pending, take them (clear to 0) and return
    /// [`WaitStep::Ready`]. Otherwise, if no one is already parked, record
    /// `caller` as the waiter and return [`WaitStep::Block`]; if another task is
    /// already parked, refuse with [`WaitStep::Busy`].
    pub fn wait(&mut self, caller: TaskId) -> WaitStep {
        if self.pending != 0 {
            let bits = self.pending;
            self.pending = 0;
            self.waiter = None;
            return WaitStep::Ready(bits);
        }
        if self.waiter.is_some() {
            return WaitStep::Busy;
        }
        self.waiter = Some(caller);
        WaitStep::Block
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wait_with_no_pending_blocks() {
        let mut n = Notification::new();
        assert_eq!(n.wait(TaskId(1)), WaitStep::Block);
    }

    #[test]
    fn signal_then_wait_returns_the_bits() {
        let mut n = Notification::new();
        assert_eq!(n.signal(0b1), SignalStep::NoWaiter);
        assert_eq!(n.wait(TaskId(1)), WaitStep::Ready(0b1));
    }

    #[test]
    fn wait_clears_pending() {
        // A second wait after the bits were taken blocks — read-and-clear, it
        // does not re-report a stale pending word.
        let mut n = Notification::new();
        let _ = n.signal(0b1);
        assert_eq!(n.wait(TaskId(1)), WaitStep::Ready(0b1));
        assert_eq!(n.wait(TaskId(1)), WaitStep::Block);
    }

    #[test]
    fn signals_coalesce() {
        // Two signals before any wait collapse into one wake of the OR-ed bits —
        // no queue, no per-signal accounting.
        let mut n = Notification::new();
        let _ = n.signal(0b001);
        let _ = n.signal(0b100);
        assert_eq!(n.wait(TaskId(1)), WaitStep::Ready(0b101));
    }

    #[test]
    fn signal_with_a_parked_waiter_wakes_it() {
        // The waiter parks, then a signal arrives — the signaller is told whom to
        // wake, and the bits remain pending for that task's re-wait.
        let mut n = Notification::new();
        assert_eq!(n.wait(TaskId(7)), WaitStep::Block);
        assert_eq!(n.signal(0b10), SignalStep::Woke(TaskId(7)));
        assert_eq!(n.wait(TaskId(7)), WaitStep::Ready(0b10));
    }

    #[test]
    fn signal_with_no_waiter_does_not_wake() {
        // Nobody parked → no one to wake, but the bits are retained.
        let mut n = Notification::new();
        assert_eq!(n.signal(0b1), SignalStep::NoWaiter);
        assert_eq!(n.wait(TaskId(1)), WaitStep::Ready(0b1));
    }

    #[test]
    fn a_second_waiter_is_refused() {
        // One task is parked; a different task waiting is refused, not silently
        // overwritten — otherwise the first parker would block forever.
        let mut n = Notification::new();
        assert_eq!(n.wait(TaskId(1)), WaitStep::Block);
        assert_eq!(n.wait(TaskId(2)), WaitStep::Busy);
        // The original waiter is intact: a signal still wakes task 1.
        assert_eq!(n.signal(0b1), SignalStep::Woke(TaskId(1)));
    }
}
