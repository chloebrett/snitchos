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
//! See `plans/legacy/spawn-shell-and-console.md`.

use alloc::collections::{BTreeMap, BTreeSet};

use crate::sched::TaskId;

/// What a `Wait` caller must do, per [`ReapTable::on_wait`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitStep {
    /// The child had already exited (and is now reaped) — return this status.
    Ready(i32),
    /// No exit yet — the caller has been recorded as the waiter and must block.
    Block,
}

/// What a `WaitAny` caller must do, per [`ReapTable::on_wait_any`] — the
/// supervising-parent variant that reaps *whichever* child exited first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitAnyStep {
    /// One of the parent's children had exited (and is now reaped) — return its
    /// id and status.
    Ready { child: TaskId, status: i32 },
    /// No child has exited — the parent is recorded as an any-waiter and blocks.
    Block,
}

/// Tracks exited-but-unreaped children (zombies) and parents blocked in `Wait`.
#[derive(Debug, Default)]
pub struct ReapTable {
    /// Children that have exited but not yet been reaped: zombie → exit status.
    exited: BTreeMap<TaskId, i32>,
    /// `child → parent` blocked waiting on it (at most one waiter per child).
    waiters: BTreeMap<TaskId, TaskId>,
    /// `child → parent` for every live spawned child — the parentage needed to
    /// match an exiting child to a parent blocked in `WaitAny`. Recorded at
    /// [`on_spawn`](Self::on_spawn), dropped when the child is reaped.
    parents: BTreeMap<TaskId, TaskId>,
    /// Parents blocked in `WaitAny` (waiting on *any* child, not a specific one).
    any_waiters: BTreeSet<TaskId>,
}

impl ReapTable {
    /// An empty table.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            exited: BTreeMap::new(),
            waiters: BTreeMap::new(),
            parents: BTreeMap::new(),
            any_waiters: BTreeSet::new(),
        }
    }

    /// `parent` waits on `child`. If the child already exited, **reap** it
    /// (remove the zombie) and return [`WaitStep::Ready`] with its status;
    /// otherwise record `parent` as the waiter on `child` and return
    /// [`WaitStep::Block`].
    pub fn on_wait(&mut self, parent: TaskId, child: TaskId) -> WaitStep {
        if let Some(status) = self.exited.remove(&child) {
            self.parents.remove(&child);
            return WaitStep::Ready(status);
        }
        self.waiters.insert(child, parent);
        WaitStep::Block
    }

    /// Record `parent → child` parentage for a freshly spawned `child`, so a
    /// later [`on_wait_any`](Self::on_wait_any) can match the child's exit to a
    /// parent waiting on *any* of its children.
    pub fn on_spawn(&mut self, parent: TaskId, child: TaskId) {
        self.parents.insert(child, parent);
    }

    /// `parent` waits for *any* of its children. If one has already exited,
    /// **reap** the lowest-id such zombie and return [`WaitAnyStep::Ready`] with
    /// its id + status; otherwise record `parent` as an any-waiter and return
    /// [`WaitAnyStep::Block`].
    pub fn on_wait_any(&mut self, parent: TaskId) -> WaitAnyStep {
        let ready = self
            .exited
            .iter()
            .find(|(child, _)| self.parents.get(child) == Some(&parent))
            .map(|(child, status)| (*child, *status));
        if let Some((child, status)) = ready {
            self.exited.remove(&child);
            self.parents.remove(&child);
            return WaitAnyStep::Ready { child, status };
        }
        self.any_waiters.insert(parent);
        WaitAnyStep::Block
    }

    /// Deregister `parent` as an any-waiter (a timed-out [`on_wait_any`](Self::on_wait_any)).
    /// Idempotent — a no-op if `parent` isn't currently any-waiting — so a racing
    /// child-exit and timeout can't leave a phantom waiter the next `on_exit` wakes.
    pub fn cancel_wait_any(&mut self, parent: TaskId) {
        self.any_waiters.remove(&parent);
    }

    /// `child` exited with `status`. Record the zombie, and return the parent to
    /// `wake` (if any): the specific waiter on `child`, or — failing that — the
    /// child's parent if it is blocked in `WaitAny`. The woken parent re-runs its
    /// `on_wait`/`on_wait_any`, finds the zombie, and reaps it.
    pub fn on_exit(&mut self, child: TaskId, status: i32) -> Option<TaskId> {
        self.exited.insert(child, status);
        if let Some(parent) = self.waiters.remove(&child) {
            return Some(parent);
        }
        let parent = *self.parents.get(&child)?;
        self.any_waiters.remove(&parent).then_some(parent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waiting_for_any_child_reaps_whichever_exited_first() {
        // A supervising parent (init) blocks for *any* of its children. The one
        // that exits first wakes it, and `wait_any` returns that child's id +
        // status.
        let mut t = ReapTable::new();
        let parent = TaskId(1);
        t.on_spawn(parent, TaskId(2));
        t.on_spawn(parent, TaskId(3));

        // No exits yet → the parent is recorded as an any-waiter and blocks.
        assert_eq!(t.on_wait_any(parent), WaitAnyStep::Block);

        // Child 3 exits first; the any-waiting parent is returned for the kernel
        // to wake.
        assert_eq!(t.on_exit(TaskId(3), 42), Some(parent));

        // The woken parent re-runs `wait_any`, finds child 3's zombie, reaps it.
        assert_eq!(
            t.on_wait_any(parent),
            WaitAnyStep::Ready { child: TaskId(3), status: 42 }
        );
    }

    #[test]
    fn cancel_wait_any_deregisters_the_parent() {
        // A timed-out `WaitAny` deregisters as an any-waiter; a later child exit
        // then wakes nobody (the parent already moved on to handle the timeout).
        let mut t = ReapTable::new();
        let parent = TaskId(1);
        t.on_spawn(parent, TaskId(2));
        assert_eq!(t.on_wait_any(parent), WaitAnyStep::Block);
        t.cancel_wait_any(parent);
        assert_eq!(t.on_exit(TaskId(2), 0), None);
    }

    #[test]
    fn wait_any_returns_a_child_that_exited_before_the_wait() {
        // A child becomes a zombie first; the parent's later wait_any reaps it
        // immediately rather than blocking.
        let mut t = ReapTable::new();
        let parent = TaskId(1);
        t.on_spawn(parent, TaskId(2));
        assert_eq!(t.on_exit(TaskId(2), 7), None); // no waiter yet
        assert_eq!(
            t.on_wait_any(parent),
            WaitAnyStep::Ready { child: TaskId(2), status: 7 }
        );
    }

    #[test]
    fn wait_any_ignores_a_different_parents_zombie() {
        // Parent 1 waits-any; an unrelated parent-9 child exits. Parent 1 must
        // not be woken or reap it — the parentage filter, not "any task exited".
        let mut t = ReapTable::new();
        t.on_spawn(TaskId(9), TaskId(2));
        assert_eq!(t.on_wait_any(TaskId(1)), WaitAnyStep::Block);
        assert_eq!(t.on_exit(TaskId(2), 0), None); // parent 9 isn't any-waiting
        assert_eq!(t.on_wait_any(TaskId(1)), WaitAnyStep::Block); // still nothing for 1
    }

    #[test]
    fn an_exit_with_no_waiter_wakes_nobody() {
        // A child with a registered parent that isn't waiting wakes no one.
        let mut t = ReapTable::new();
        t.on_spawn(TaskId(1), TaskId(2));
        assert_eq!(t.on_exit(TaskId(2), 0), None);
    }

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
