//! Scheduler data structures. Pure FIFO Runqueue over `TaskId`s for
//! v0.5's cooperative round-robin scheduler. The kernel binary owns
//! the actual task list, stacks, and the runqueue-as-`Mutex`-protected
//! static — this module is pure bookkeeping.
//!
//! See `plans/v0.5-threading.md`.

use alloc::collections::VecDeque;

/// Identifier for a task. Allocated by the kernel-side task table;
/// kernel-core treats it as an opaque newtype.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct TaskId(pub u32);

/// FIFO queue of `Ready` tasks. Cooperative round-robin scheduler
/// pops from front, runs the task, then pushes the same task back
/// onto the end if it's still ready.
pub struct Runqueue {
    ready: VecDeque<TaskId>,
}

impl Runqueue {
    pub fn new() -> Self {
        Self { ready: VecDeque::new() }
    }

    pub fn len(&self) -> usize {
        self.ready.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ready.is_empty()
    }

    pub fn push_back(&mut self, id: TaskId) {
        self.ready.push_back(id);
    }

    pub fn pop_front(&mut self) -> Option<TaskId> {
        self.ready.pop_front()
    }
}

impl Default for Runqueue {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_runqueue_is_empty() {
        let q = Runqueue::new();
        assert!(q.is_empty());
        assert_eq!(q.len(), 0);
    }

    #[test]
    fn pop_from_empty_returns_none() {
        let mut q = Runqueue::new();
        assert_eq!(q.pop_front(), None);
    }

    #[test]
    fn push_then_pop_returns_same_task() {
        let mut q = Runqueue::new();
        q.push_back(TaskId(7));
        assert!(!q.is_empty());
        assert_eq!(q.len(), 1);
        assert_eq!(q.pop_front(), Some(TaskId(7)));
        assert!(q.is_empty());
    }

    #[test]
    fn pop_order_is_fifo() {
        // Three tasks pushed in order should pop in the same order
        // — round-robin fairness depends on this.
        let mut q = Runqueue::new();
        q.push_back(TaskId(1));
        q.push_back(TaskId(2));
        q.push_back(TaskId(3));
        assert_eq!(q.pop_front(), Some(TaskId(1)));
        assert_eq!(q.pop_front(), Some(TaskId(2)));
        assert_eq!(q.pop_front(), Some(TaskId(3)));
        assert_eq!(q.pop_front(), None);
    }

    #[test]
    fn pop_then_push_back_implements_round_robin_rotation() {
        // The scheduler's core idiom: pop the head, run it, push it
        // back at the tail. Repeating that should cycle through every
        // task in original order indefinitely.
        let mut q = Runqueue::new();
        q.push_back(TaskId(1));
        q.push_back(TaskId(2));
        q.push_back(TaskId(3));

        let first = q.pop_front().unwrap();
        q.push_back(first);
        let second = q.pop_front().unwrap();
        q.push_back(second);
        let third = q.pop_front().unwrap();
        q.push_back(third);

        // After three rotations, the head should be task 1 again.
        assert_eq!(q.pop_front(), Some(TaskId(1)));
    }

    #[test]
    fn len_tracks_pushes_and_pops() {
        let mut q = Runqueue::new();
        assert_eq!(q.len(), 0);
        q.push_back(TaskId(10));
        assert_eq!(q.len(), 1);
        q.push_back(TaskId(11));
        assert_eq!(q.len(), 2);
        q.pop_front();
        assert_eq!(q.len(), 1);
        q.pop_front();
        assert_eq!(q.len(), 0);
    }

    #[test]
    fn task_id_round_trips_through_queue_value() {
        // Specific id values shouldn't be transformed by the queue —
        // we want exact-equality semantics, not "some TaskId came out."
        let mut q = Runqueue::new();
        q.push_back(TaskId(0));
        q.push_back(TaskId(u32::MAX));
        assert_eq!(q.pop_front(), Some(TaskId(0)));
        assert_eq!(q.pop_front(), Some(TaskId(u32::MAX)));
    }
}
