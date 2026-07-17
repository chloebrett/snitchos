//! The **timeout queue** — the pure core of timed waits (v2b hung detection).
//!
//! A blocked waiter (a timed `WaitNotify`/`WaitAny`) registers an absolute-tick
//! `deadline`; the owning hart's timer IRQ [`drain_expired`](TimeoutQueue::drain_expired)s
//! every tick and wakes each task whose deadline has passed, so its wait loop
//! re-checks and returns a `TimedOut` result. Pure data + bookkeeping, host-tested
//! like [`crate::reap`] / [`crate::notify`]; the kernel owns the per-hart live queue
//! (behind a `Mutex`) and the `wake` wiring.
//!
//! Ordered by `(deadline, task)` so the earliest deadline is the min and expiry is a
//! single `split_off`. A task waits on at most one thing at a time, so it holds at
//! most one entry; [`remove`](TimeoutQueue::remove) (a normal wake beat the deadline)
//! and [`insert`](TimeoutQueue::insert) are the only mutators besides the drain.

use alloc::collections::BTreeSet;
use alloc::vec::Vec;

use crate::sched::TaskId;

/// Absolute-tick deadlines keyed to the tasks awaiting them.
#[derive(Debug, Default)]
pub struct TimeoutQueue {
    /// `(deadline, task)` — ordered so `drain_expired` is a range split and the
    /// earliest deadline is `first()`.
    entries: BTreeSet<(u64, TaskId)>,
}

impl TimeoutQueue {
    /// An empty queue.
    #[must_use]
    pub const fn new() -> Self {
        Self { entries: BTreeSet::new() }
    }

    /// Register `task` to be woken at absolute tick `deadline`.
    pub fn insert(&mut self, deadline: u64, task: TaskId) {
        self.entries.insert((deadline, task));
    }

    /// Cancel any pending deadline for `task` (its wait completed before timing
    /// out). Idempotent — a no-op if `task` isn't queued.
    pub fn remove(&mut self, task: TaskId) {
        self.entries.retain(|(_, t)| *t != task);
    }

    /// Remove and return every task whose deadline has passed (`deadline <= now`).
    /// The earliest deadlines come first; the queue keeps only future entries.
    pub fn drain_expired(&mut self, now: u64) -> Vec<TaskId> {
        // Everything strictly before `(now + 1, 0)` has `deadline <= now`.
        let boundary = (now.saturating_add(1), TaskId(0));
        let future = self.entries.split_off(&boundary);
        let expired = self.entries.iter().map(|(_, task)| *task).collect();
        self.entries = future;
        expired
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_expired_deadline_drains() {
        let mut q = TimeoutQueue::new();
        q.insert(100, TaskId(1));
        assert_eq!(q.drain_expired(100), alloc::vec![TaskId(1)]);
    }

    #[test]
    fn a_future_deadline_stays() {
        // `now` before the deadline drains nothing and leaves the entry queued.
        let mut q = TimeoutQueue::new();
        q.insert(100, TaskId(1));
        assert!(q.drain_expired(99).is_empty());
        // ...and it still fires once its time comes.
        assert_eq!(q.drain_expired(100), alloc::vec![TaskId(1)]);
    }

    #[test]
    fn drain_returns_earliest_deadlines_first() {
        let mut q = TimeoutQueue::new();
        q.insert(300, TaskId(3));
        q.insert(100, TaskId(1));
        q.insert(200, TaskId(2));
        // now = 250 expires 100 and 200 (in deadline order), leaves 300.
        assert_eq!(q.drain_expired(250), alloc::vec![TaskId(1), TaskId(2)]);
        assert_eq!(q.drain_expired(300), alloc::vec![TaskId(3)]);
    }

    #[test]
    fn remove_cancels_a_pending_deadline() {
        // A wait woken by its event (before the deadline) deregisters — the timer
        // must not later "time out" an already-completed wait.
        let mut q = TimeoutQueue::new();
        q.insert(100, TaskId(1));
        q.remove(TaskId(1));
        assert!(q.drain_expired(100).is_empty());
    }

    #[test]
    fn remove_is_idempotent_for_an_absent_task() {
        let mut q = TimeoutQueue::new();
        q.remove(TaskId(9)); // never inserted — no panic, no effect
        q.insert(100, TaskId(1));
        assert_eq!(q.drain_expired(100), alloc::vec![TaskId(1)]);
    }

    #[test]
    fn several_tasks_can_share_a_deadline() {
        let mut q = TimeoutQueue::new();
        q.insert(100, TaskId(1));
        q.insert(100, TaskId(2));
        assert_eq!(q.drain_expired(100), alloc::vec![TaskId(1), TaskId(2)]);
    }
}
