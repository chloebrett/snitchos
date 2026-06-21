//! Wait/exit **reaping** bookkeeping — the pure v0.12 process-lifecycle core.
//!
//! A parent `Wait`s for a child to `Exit`. This tracks the two states that
//! creates: **zombies** (a child that has exited but whose status nobody has
//! collected yet) and **waiters** (a parent blocked on a specific child that
//! hasn't exited). Pure data + bookkeeping, host-tested like [`crate::ipc`] and
//! [`crate::sched`]; the kernel owns the live table (behind a `Mutex`) and the
//! `block_current`/`wake` wiring.
//!
//! The two halves mirror each other:
//! - [`ReapTable::on_wait`] — child already a zombie? reap it (return status).
//!   Else record the waiter and block.
//! - [`ReapTable::on_exit`] — record the zombie; return the parent (if any) to
//!   wake. The woken parent re-runs `on_wait`, finds the zombie, and reaps it.
//!
//! See `plans/spawn-shell-and-console.md`.

use alloc::collections::BTreeMap;

use crate::sched::TaskId;

/// What a `Wait` caller must do, per [`ReapTable::on_wait`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitStep {
    /// The child had already exited (and is now reaped) — return this status.
    Ready(i32),
    /// No exit yet — the caller has been recorded as the waiter and must block.
    Block,
}

/// Tracks exited-but-unreaped children (zombies) and parents blocked in `Wait`.
#[derive(Debug, Default)]
pub struct ReapTable {
    /// Children that have exited but not yet been reaped: zombie → exit status.
    exited: BTreeMap<TaskId, i32>,
    /// `child → parent` blocked waiting on it (at most one waiter per child).
    waiters: BTreeMap<TaskId, TaskId>,
}

impl ReapTable {
    /// An empty table.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            exited: BTreeMap::new(),
            waiters: BTreeMap::new(),
        }
    }

    /// `parent` waits on `child`. If the child already exited, **reap** it
    /// (remove the zombie) and return [`WaitStep::Ready`] with its status;
    /// otherwise record `parent` as the waiter on `child` and return
    /// [`WaitStep::Block`].
    pub fn on_wait(&mut self, parent: TaskId, child: TaskId) -> WaitStep {
        if let Some(status) = self.exited.remove(&child) {
            return WaitStep::Ready(status);
        }
        self.waiters.insert(child, parent);
        WaitStep::Block
    }

    /// `child` exited with `status`. Record the zombie, and return the parent
    /// blocked on it (if any) for the kernel to `wake` — that parent re-runs
    /// `on_wait`, finds the zombie, and reaps it.
    pub fn on_exit(&mut self, child: TaskId, status: i32) -> Option<TaskId> {
        self.exited.insert(child, status);
        self.waiters.remove(&child)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waiting_on_a_live_child_blocks() {
        let mut t = ReapTable::new();
        assert_eq!(t.on_wait(TaskId(1), TaskId(2)), WaitStep::Block);
    }

    #[test]
    fn a_child_exiting_with_no_waiter_becomes_a_zombie() {
        // No one is waiting yet → no parent to wake; the status is held until a
        // later `wait` reaps it.
        let mut t = ReapTable::new();
        assert_eq!(t.on_exit(TaskId(2), 7), None);
        assert_eq!(t.on_wait(TaskId(1), TaskId(2)), WaitStep::Ready(7));
    }

    #[test]
    fn a_child_exiting_with_a_waiter_returns_the_parent_to_wake() {
        let mut t = ReapTable::new();
        assert_eq!(t.on_wait(TaskId(1), TaskId(2)), WaitStep::Block);
        assert_eq!(t.on_exit(TaskId(2), 0), Some(TaskId(1)));
        // The woken parent re-runs on_wait and reaps the zombie.
        assert_eq!(t.on_wait(TaskId(1), TaskId(2)), WaitStep::Ready(0));
    }

    #[test]
    fn the_exit_status_round_trips() {
        let mut t = ReapTable::new();
        let _ = t.on_exit(TaskId(2), 42);
        assert_eq!(t.on_wait(TaskId(1), TaskId(2)), WaitStep::Ready(42));
    }

    #[test]
    fn reaping_consumes_the_zombie() {
        // Once reaped, the status is gone — a second wait on the same id blocks
        // (the child won't exit twice), it doesn't re-report a stale status.
        let mut t = ReapTable::new();
        let _ = t.on_exit(TaskId(2), 5);
        assert_eq!(t.on_wait(TaskId(1), TaskId(2)), WaitStep::Ready(5));
        assert_eq!(t.on_wait(TaskId(1), TaskId(2)), WaitStep::Block);
    }
}
