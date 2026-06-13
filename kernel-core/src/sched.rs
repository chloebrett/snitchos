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

/// Where a task is in the scheduler's lifecycle.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TaskState {
    /// On the runqueue, waiting for a turn on the CPU.
    Ready,
    /// Currently on-CPU.
    Running,
    /// Off-CPU, waiting on something. Placeholder until v0.5.x adds
    /// real blocking primitives.
    Blocked,
    /// Entry function returned (today: impossible since `fn() -> !`,
    /// but placeholder so the state machine is complete).
    Exited,
}

/// FIFO queue of `Ready` tasks. Cooperative round-robin scheduler
/// pops from front, runs the task, then pushes the same task back
/// onto the end if it's still ready.
pub struct Runqueue {
    ready: VecDeque<TaskId>,
}

impl Runqueue {
    pub const fn new() -> Self {
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

/// Decide whether switching into a task requires reloading `satp`, and to
/// which root page-table PA.
///
/// `active_root` is the root currently loaded in `satp`. `next_root` is the
/// incoming task's user address space, or `None` for a kernel task — which
/// runs correctly under whatever root is loaded, because the kernel high-half
/// is mapped into every address space.
///
/// Returns `Some(root)` when the caller must write `root` to `satp` (and
/// `sfence.vma`), or `None` when the loaded `satp` already serves the next
/// task. Pure so the policy is host-tested away from the asm; the kernel-side
/// `yield_now` reads the result and does the CSR write.
#[must_use]
pub fn address_space_switch(active_root: usize, next_root: Option<usize>) -> Option<usize> {
    match next_root {
        Some(root) if root != active_root => Some(root),
        _ => None,
    }
}

/// Whether the running task has used up its time slice and should be preempted.
///
/// `entry_tick` is when the task last became Running; `now` the current clock;
/// `quantum` the slice length (both in the same timer-tick unit). Returns
/// `true` once at least a full `quantum` has elapsed (`>=`, so a task can't
/// straddle the boundary forever). A non-monotonic clock (`now < entry_tick` —
/// wraparound or a clock that went backwards) returns `false` rather than
/// panicking on overflow: never preempt on a bogus elapsed time.
///
/// Pure so the policy is host-tested away from the timer handler (mirrors
/// `heap::watermark_grow_decision`); the kernel passes its `QUANTUM_TICKS`.
#[must_use]
pub fn quantum_expired(entry_tick: u64, now: u64, quantum: u64) -> bool {
    match now.checked_sub(entry_tick) {
        Some(elapsed) => elapsed >= quantum,
        None => false,
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

    #[test]
    fn switch_into_kernel_task_keeps_current_satp() {
        // A kernel task (no address space) runs under whatever root is
        // loaded — the kernel high-half is mapped into every space — so
        // no `satp` write is needed.
        assert_eq!(address_space_switch(0xA000, None), None);
    }

    #[test]
    fn switch_into_same_address_space_is_a_noop() {
        // Switching into a user task whose root is already in `satp`
        // (e.g. kernel idle ran under it in between) needs no reload —
        // avoids a redundant TLB flush.
        assert_eq!(address_space_switch(0xB000, Some(0xB000)), None);
    }

    #[test]
    fn switch_into_different_address_space_loads_the_next_root() {
        // The crux: switching from one user process to another with a
        // distinct root must load the *next* task's root into `satp`.
        assert_eq!(address_space_switch(0xB000, Some(0xC000)), Some(0xC000));
    }

    #[test]
    fn switch_returns_the_next_root_not_the_active_one() {
        // Guards against returning `active_root` — the value written to
        // `satp` must be the incoming task's address space.
        assert_eq!(address_space_switch(0x1000, Some(0x2000)), Some(0x2000));
    }

    #[test]
    fn quantum_not_expired_below_the_slice() {
        // Less than a full quantum on-CPU — the task keeps running.
        assert!(!quantum_expired(100, 105, 10));
    }

    #[test]
    fn quantum_expired_at_the_boundary() {
        // Exactly one quantum elapsed counts as expired (`>=`), so a task
        // can't run a hair past its slice forever by landing on the boundary.
        assert!(quantum_expired(100, 110, 10));
    }

    #[test]
    fn quantum_expired_past_the_slice() {
        assert!(quantum_expired(100, 200, 10));
    }

    #[test]
    fn quantum_not_expired_at_entry_instant() {
        // Zero time elapsed — definitely not expired.
        assert!(!quantum_expired(100, 100, 10));
    }

    #[test]
    fn quantum_non_monotonic_clock_does_not_expire() {
        // If `now` is before `entry` (clock went backwards / wraparound),
        // never report expiry and never panic — guard, don't subtract-overflow.
        assert!(!quantum_expired(100, 50, 10));
    }
}
