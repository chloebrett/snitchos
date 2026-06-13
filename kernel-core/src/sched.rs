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

/// Static scheduling priority (v0.8b). Higher runs first; `Normal` is the
/// default. Discriminants are the level used by [`aged_priority`] arithmetic —
/// `High` is the ceiling aging saturates at.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Priority {
    Low = 0,
    Normal = 1,
    High = 2,
}

/// Effective priority *level* of a ready task, accounting for aging: its base
/// level plus one level per `step` ticks it has waited, saturating at
/// [`Priority::High`]. This is what keeps low-priority tasks from starving — the
/// longer one waits, the higher it effectively bids for the CPU.
///
/// `step == 0` disables aging (no boost, never panics — no division by zero).
/// Pure so the policy is host-tested off-target; [`pick_next`] composes it.
#[must_use]
pub fn aged_priority(base: Priority, waited_ticks: u64, step: u64) -> u8 {
    let boost = if step == 0 { 0 } else { waited_ticks / step };
    ((base as u64) + boost).min(Priority::High as u64) as u8
}

/// A ready task the scheduler may pick: its id, base priority, and when it last
/// entered the ready set (for aging).
#[derive(Clone, Copy, Debug)]
pub struct Candidate {
    pub id: TaskId,
    pub base: Priority,
    pub enqueued_tick: u64,
}

/// Choose the next task to run from the ready set: the one with the highest
/// *effective* ([`aged_priority`]) level, ties broken by longest wait (smallest
/// `enqueued_tick`) so equal-priority tasks round-robin fairly. `None` if the
/// ready set is empty.
///
/// `waited = now - enqueued_tick`, saturating at 0 if the clock is non-monotonic
/// (never panics, never boosts on a bogus wait). Pure: the kernel builds
/// `candidates` from its task table and runs the winner.
#[must_use]
pub fn pick_next(candidates: &[Candidate], now: u64, step: u64) -> Option<TaskId> {
    candidates
        .iter()
        .max_by_key(|c| {
            let waited = now.saturating_sub(c.enqueued_tick);
            // Higher effective priority wins; among equals, the longest waiter
            // (smallest enqueued_tick → largest `Reverse`) wins.
            (aged_priority(c.base, waited, step), core::cmp::Reverse(c.enqueued_tick))
        })
        .map(|c| c.id)
}

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

    #[test]
    fn aging_no_boost_below_a_step() {
        // Waited less than one step — effective priority is still the base.
        assert_eq!(aged_priority(Priority::Low, 9, 10), Priority::Low as u8);
    }

    #[test]
    fn aging_one_boost_at_the_step_boundary() {
        // Exactly one step waited → one level of boost.
        assert_eq!(aged_priority(Priority::Low, 10, 10), Priority::Normal as u8);
    }

    #[test]
    fn aging_saturates_at_high() {
        // A Low task waiting many steps can't exceed High — no runaway boost.
        assert_eq!(aged_priority(Priority::Low, 100, 10), Priority::High as u8);
    }

    #[test]
    fn aging_a_high_task_stays_high() {
        assert_eq!(aged_priority(Priority::High, 50, 10), Priority::High as u8);
    }

    #[test]
    fn aging_normal_reaches_high_after_one_step() {
        assert_eq!(aged_priority(Priority::Normal, 10, 10), Priority::High as u8);
    }

    #[test]
    fn aging_disabled_when_step_is_zero() {
        // `step == 0` must not divide-by-zero; it disables aging entirely.
        assert_eq!(aged_priority(Priority::Low, 1_000_000, 0), Priority::Low as u8);
    }

    fn cand(id: u32, base: Priority, enqueued_tick: u64) -> Candidate {
        Candidate { id: TaskId(id), base, enqueued_tick }
    }

    #[test]
    fn pick_from_empty_is_none() {
        assert_eq!(pick_next(&[], 100, 10), None);
    }

    #[test]
    fn pick_single_candidate_returns_it() {
        assert_eq!(pick_next(&[cand(7, Priority::Low, 0)], 100, 10), Some(TaskId(7)));
    }

    #[test]
    fn pick_highest_base_priority_when_all_fresh() {
        // All enqueued "now" → no aging → pure base priority decides.
        let cs = [
            cand(1, Priority::Low, 100),
            cand(2, Priority::High, 100),
            cand(3, Priority::Normal, 100),
        ];
        assert_eq!(pick_next(&cs, 100, 10), Some(TaskId(2)));
    }

    #[test]
    fn pick_aged_low_overtakes_fresh_normal() {
        // A Low task that has waited two steps ages to High and beats a freshly
        // enqueued Normal — this is the anti-starvation guarantee.
        let cs = [
            cand(1, Priority::Normal, 100), // fresh → effective Normal
            cand(2, Priority::Low, 80),     // waited 20 = 2 steps → effective High
        ];
        assert_eq!(pick_next(&cs, 100, 10), Some(TaskId(2)));
    }

    #[test]
    fn pick_breaks_ties_by_longest_wait() {
        // Equal effective priority → the task that has waited longest (smallest
        // enqueued_tick) runs, giving round-robin fairness within a level.
        let cs = [
            cand(1, Priority::Normal, 90),
            cand(2, Priority::Normal, 50), // waited longer
            cand(3, Priority::Normal, 70),
        ];
        assert_eq!(pick_next(&cs, 100, 1000), Some(TaskId(2)));
    }

    #[test]
    fn pick_non_monotonic_wait_does_not_panic_or_boost() {
        // Task 1 was enqueued in the "future" (now < enqueued): the wait must
        // saturate to 0 — no boost, no overflow/panic — leaving it at its base
        // High. Task 2 waited only 5 < step, so it stays Low. High wins.
        let cs = [
            cand(1, Priority::High, 200), // enqueued after `now` → wait saturates to 0
            cand(2, Priority::Low, 95),   // waited 5 < step → no boost → Low
        ];
        assert_eq!(pick_next(&cs, 100, 10), Some(TaskId(1)));
    }
}
