//! Scheduler data structures. Pure FIFO Runqueue over `TaskId`s for
//! v0.5's cooperative round-robin scheduler. The kernel binary owns
//! the actual task list, stacks, and the runqueue-as-`Mutex`-protected
//! static — this module is pure bookkeeping.
//!
//! See `plans/legacy/v0.5-threading.md`.

use alloc::collections::{BTreeMap, VecDeque};

/// Identifier for a task. Allocated by the kernel-side task table;
/// kernel-core treats it as an opaque newtype.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct TaskId(pub u32);

/// `TaskId → slot` index into the kernel's `tasks: Vec<Box<Task>>`, so a lookup by
/// id is a map probe instead of an O(tasks) linear scan of the table. Maintained in
/// lockstep with the `Vec` under the scheduler lock: [`insert`](Self::insert) on
/// `push`, [`swap_remove`](Self::swap_remove) mirroring `Vec::swap_remove`. Pure
/// bookkeeping — the kernel owns the actual `Vec`; this only tracks where each id
/// lives, so `prepare_switch` touches exactly the two tasks it switches between.
#[derive(Debug, Default, Clone)]
pub struct TaskDirectory {
    slots: BTreeMap<u32, usize>,
}

impl TaskDirectory {
    #[must_use]
    pub const fn new() -> Self {
        Self { slots: BTreeMap::new() }
    }

    /// Record that `id` now lives at `slot` (= the `Vec` len just before `push`).
    pub fn insert(&mut self, id: TaskId, slot: usize) {
        self.slots.insert(id.0, slot);
    }

    /// The slot `id` occupies, or `None` if unknown.
    #[must_use]
    pub fn slot_of(&self, id: TaskId) -> Option<usize> {
        self.slots.get(&id.0).copied()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Mirror `Vec::swap_remove(slot_of(removed))`: `removed` is dropped and the
    /// element that was last (`moved`) takes its slot. Pass `moved == removed` when
    /// `removed` was itself the last element (nothing moves). A no-op for an unknown
    /// `removed`.
    pub fn swap_remove(&mut self, removed: TaskId, moved: TaskId) {
        if let Some(slot) = self.slots.remove(&removed.0)
            && moved != removed
        {
            self.slots.insert(moved.0, slot);
        }
    }
}

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

/// Whether the timer should preempt the running task (v0.8b, priority-aware).
///
/// Returns `true` only if some ready task has an *effective* ([`aged_priority`])
/// level `>=` the running task's `current_level`. The timer thus time-slices
/// *within* a priority level (an equal-priority peer triggers a switch) and
/// honours a higher-priority arrival, but never **demotes** a higher-priority
/// task to a lower-priority one — that would be priority inversion. Aging is
/// what lets a long-starved low task eventually reach the running level and so
/// preempt even a higher-priority CPU hog.
///
/// `current_level` is the running task's base level (it's on-CPU, so wait = 0
/// and `aged == base`). Pure; the kernel passes its ready queue + step.
#[must_use]
pub fn should_preempt(
    current_level: u8,
    ready: impl IntoIterator<Item = Candidate>,
    now: u64,
    step: u64,
) -> bool {
    ready
        .into_iter()
        .any(|c| aged_priority(c.base, now.saturating_sub(c.enqueued_tick), step) >= current_level)
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
pub fn pick_next(
    candidates: impl IntoIterator<Item = Candidate>,
    now: u64,
    step: u64,
) -> Option<TaskId> {
    candidates
        .into_iter()
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

/// What happens to the *currently running* task when the scheduler switches
/// away from it — the one axis on which the kernel's `reschedule` /
/// `block_current` / `exit_now` differ. Everything else about a switch (pick
/// next, load its state, account, emit the `ContextSwitch`) is identical.
///
/// Pure so the state-transition + re-enqueue policy is host-tested away from
/// the switch path; the kernel-side `prepare_switch` reads [`next_state`] to set
/// the outgoing task's `state` and [`requeues`] to decide whether to push it
/// back onto the ready set.
///
/// [`next_state`]: CurrentDisposition::next_state
/// [`requeues`]: CurrentDisposition::requeues
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CurrentDisposition {
    /// Voluntary/involuntary deschedule: the current task re-enters the ready
    /// set (`yield_now`/preemption). Its `state` field is left as-is — the
    /// runqueue membership is the source of truth.
    Requeue,
    /// The current task blocks: marked `Blocked` and left **off** the runqueue
    /// until a `wake` returns it (`block_current`).
    Block,
    /// The current task terminates: marked `Exited` and left off the runqueue
    /// forever (`exit_now`).
    Exit,
}

impl CurrentDisposition {
    /// The state the outgoing task should transition to, or `None` to leave its
    /// `state` field unchanged (a [`Requeue`](Self::Requeue), whose source of
    /// truth is runqueue membership, not the field).
    #[must_use]
    pub fn next_state(self) -> Option<TaskState> {
        match self {
            Self::Requeue => None,
            Self::Block => Some(TaskState::Blocked),
            Self::Exit => Some(TaskState::Exited),
        }
    }

    /// Whether the outgoing task re-enters the ready set. Only a
    /// [`Requeue`](Self::Requeue) does; [`Block`](Self::Block) waits for a wake
    /// and [`Exit`](Self::Exit) is gone for good.
    #[must_use]
    pub fn requeues(self) -> bool {
        matches!(self, Self::Requeue)
    }
}

/// Ready set for one hart. Stores each ready task as a [`Candidate`] —
/// `(id, base priority, enqueued_tick)` — so the scheduler's priority pick
/// ([`pick_next`]) reads everything it needs straight from the queue, with no
/// per-task table lookup (that lookup was an O(n²)-per-switch trap). Insertion
/// order is preserved; the *pick* is by effective priority, not position.
pub struct Runqueue {
    ready: VecDeque<Candidate>,
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

    /// Add a ready task. The caller stamps `enqueued_tick` (= now) so aging
    /// measures the wait from this enqueue.
    pub fn push_back(&mut self, candidate: Candidate) {
        self.ready.push_back(candidate);
    }

    pub fn pop_front(&mut self) -> Option<Candidate> {
        self.ready.pop_front()
    }

    /// Remove a specific task wherever it sits in the queue — the scheduler
    /// picked it by effective priority, not FIFO position. Returns whether it
    /// was present. O(n) in the queue length (small: the ready set on one hart).
    pub fn remove(&mut self, id: TaskId) -> bool {
        if let Some(pos) = self.ready.iter().position(|c| c.id == id) {
            self.ready.remove(pos);
            true
        } else {
            false
        }
    }

    /// Iterate the ready candidates (insertion order) for the priority pick,
    /// without draining the queue.
    pub fn iter(&self) -> impl Iterator<Item = Candidate> + '_ {
        self.ready.iter().copied()
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

/// On-CPU time a task accrued during its turn, from when it became Running
/// (`entry_tick`) to when it switched out (`now`), in `time`-CSR ticks.
///
/// `entry_tick == 0` is the "no entry recorded yet" sentinel (the scheduler
/// lazy-inits the per-task entry tick on the first switch rather than during
/// boot): it yields `0` so a task isn't credited the whole uptime on its first
/// deschedule. Otherwise the delta wraps, since the 64-bit `time` CSR wraps and
/// a span straddling that boundary must still measure the true elapsed time
/// rather than overflow-panic.
///
/// Pure so the accounting is host-tested away from the switch path; the
/// kernel-side `prepare_switch` adds the result to the outgoing task's counter.
#[must_use]
pub fn on_cpu_delta(entry_tick: u64, now: u64) -> u64 {
    if entry_tick == 0 {
        0
    } else {
        now.wrapping_sub(entry_tick)
    }
}

/// Whether a `wake` should re-enqueue the task — true only when it is
/// actually [`TaskState::Blocked`]. The idempotence guard for `wake`: a
/// second wake (or one that races the task already back on the runqueue)
/// finds it non-`Blocked` and is a no-op, so a task is never double-enqueued.
/// Pure so the guard is host-tested away from the scheduler lock.
#[must_use]
pub fn on_wake(state: TaskState) -> bool {
    matches!(state, TaskState::Blocked)
}

/// What the kernel-side `kill_task` should do with a target, decided purely from
/// its scheduler placement. The v2a `Kill` primitive terminates a task that is
/// **not** the one running; where the target's id lives — and whether reaping it
/// now is safe — is the whole design (see `plans/supervision-v2.md` §3a).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum KillAction {
    /// The target already `Exited` — nothing to do; the kill succeeds idempotently.
    NoOp,
    /// The target is off-CPU (`Ready` on a runqueue **or** `Blocked` in a wait
    /// structure): terminate it in place — remove it from any runqueue, cancel any
    /// endpoint wait, zombify. Safe because a non-running target's stack + `satp`
    /// are quiescent; the only hazard, a ghost id left in an endpoint queue, is
    /// cleared by `ipc::cancel_wait` (inc 3.5).
    Terminate,
    /// The target is the caller itself — that's `Exit`'s job, not `Kill`'s.
    RefuseSelf,
    /// The target is running on another hart: a live stack + loaded `satp` make an
    /// out-of-band reap a UAF. Deferred to v2b (needs an IPI to halt it).
    RefuseRunningRemote,
}

/// The scheduler placement of a kill target — the inputs [`classify_kill`] reads,
/// named so call sites don't pass a row of ambiguous bools (boolean blindness).
/// `Default` is all-false = an off-CPU (`Ready`-or-`Blocked`) live target, the base
/// case, so a test names only the axis it exercises: `KillTarget { is_self: true,
/// ..Default::default() }`. Note `ready` is *not* an input: a non-running,
/// non-exited target terminates whether `Ready` or `Blocked` (the runqueue-vs-
/// endpoint cleanup is mechanism, done unconditionally in `kill_task`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct KillTarget {
    /// The target *is* the calling task (the runner on this hart).
    pub is_self: bool,
    /// The target is currently on-CPU on a **different** hart.
    pub running_remote: bool,
    /// The target has already `Exited` (kill is then an idempotent no-op).
    pub is_exited: bool,
}

/// Decide how to kill a target, given its scheduler placement. Pure so the v2a
/// scope (terminate any off-CPU target; defer only cross-hart-running) is
/// host-tested away from the kernel's runqueue/`CURRENT_TASK`/endpoint plumbing.
///
/// Precedence is self → running-remote → exited → terminate. Self is decided first
/// so the caller (which is the running task on this hart) never routes to a kill
/// path; `running_remote` comes before `is_exited` because the kernel derives
/// running from `CURRENT_TASK`, not the `state` field, which does not reliably track
/// `Running` (an incoming task is never re-labelled).
#[must_use]
pub fn classify_kill(target: KillTarget) -> KillAction {
    let KillTarget { is_self, running_remote, is_exited } = target;
    // Arm order *is* the precedence — the first matching arm wins. The trailing arm
    // (not self / not running-remote / not exited) is an off-CPU live target, `Ready`
    // or `Blocked`; both terminate now (inc 3.5 collapsed the blocked case).
    match (is_self, running_remote, is_exited) {
        (true, _, _) => KillAction::RefuseSelf,
        (_, true, _) => KillAction::RefuseRunningRemote,
        (_, _, true) => KillAction::NoOp,
        _ => KillAction::Terminate,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directory_resolves_ids_to_slots() {
        let mut dir = TaskDirectory::new();
        dir.insert(TaskId(10), 0);
        dir.insert(TaskId(20), 1);
        dir.insert(TaskId(30), 2);
        assert_eq!(dir.slot_of(TaskId(10)), Some(0));
        assert_eq!(dir.slot_of(TaskId(30)), Some(2));
        assert_eq!(dir.slot_of(TaskId(99)), None);
        assert_eq!(dir.len(), 3);
    }

    #[test]
    fn directory_swap_remove_repoints_the_moved_id() {
        // Mirrors `Vec::swap_remove(0)` on [10,20,30]: 30 moves into slot 0.
        let mut dir = TaskDirectory::new();
        dir.insert(TaskId(10), 0);
        dir.insert(TaskId(20), 1);
        dir.insert(TaskId(30), 2);
        dir.swap_remove(TaskId(10), TaskId(30)); // removed=10 (slot 0), moved=30 (was last)
        assert_eq!(dir.slot_of(TaskId(10)), None);
        assert_eq!(dir.slot_of(TaskId(30)), Some(0)); // repointed to the freed slot
        assert_eq!(dir.slot_of(TaskId(20)), Some(1)); // untouched
        assert_eq!(dir.len(), 2);
    }

    #[test]
    fn directory_swap_remove_of_the_last_element_moves_nothing() {
        // `Vec::swap_remove(last)`: no element moves, so moved == removed.
        let mut dir = TaskDirectory::new();
        dir.insert(TaskId(10), 0);
        dir.insert(TaskId(20), 1);
        dir.swap_remove(TaskId(20), TaskId(20));
        assert_eq!(dir.slot_of(TaskId(20)), None);
        assert_eq!(dir.slot_of(TaskId(10)), Some(0));
        assert_eq!(dir.len(), 1);
    }

    #[test]
    fn requeued_task_keeps_its_state() {
        // A yielded/preempted task re-enters the ready set; runqueue membership
        // is the source of truth, so its `state` field is left untouched.
        assert_eq!(CurrentDisposition::Requeue.next_state(), None);
    }

    #[test]
    fn blocked_task_transitions_to_blocked() {
        assert_eq!(CurrentDisposition::Block.next_state(), Some(TaskState::Blocked));
    }

    #[test]
    fn exited_task_transitions_to_exited() {
        assert_eq!(CurrentDisposition::Exit.next_state(), Some(TaskState::Exited));
    }

    #[test]
    fn only_a_requeue_returns_to_the_ready_set() {
        // Re-enqueue is exactly the Requeue disposition: Block and Exit leave the
        // task off the runqueue (a wake / nothing returns them, respectively).
        assert!(CurrentDisposition::Requeue.requeues());
        assert!(!CurrentDisposition::Block.requeues());
        assert!(!CurrentDisposition::Exit.requeues());
    }

    #[test]
    fn on_cpu_delta_is_zero_for_the_uninitialised_entry_sentinel() {
        // `prev_entry == 0` means "this task has no recorded entry tick yet"
        // (lazy-init on the first switch). Accruing `now - 0` would credit the
        // task with the entire uptime; the sentinel must yield zero instead.
        assert_eq!(on_cpu_delta(0, 1_000_000), 0);
    }

    #[test]
    fn on_cpu_delta_is_the_elapsed_time_since_entry() {
        assert_eq!(on_cpu_delta(100, 150), 50);
    }

    #[test]
    fn on_cpu_delta_wraps_rather_than_overflowing() {
        // The `time` CSR is 64-bit and wraps; a span straddling the wrap point
        // must compute the true elapsed delta, not panic on overflow.
        assert_eq!(on_cpu_delta(u64::MAX, 4), 5);
    }

    #[test]
    fn a_blocked_task_is_enqueued_when_woken() {
        assert!(on_wake(TaskState::Blocked));
    }

    #[test]
    fn waking_a_non_blocked_task_is_a_no_op() {
        // The idempotence guard: a wake that races a task already back on
        // the runqueue (Ready), on-CPU (Running), or gone (Exited) must NOT
        // re-enqueue it — a double enqueue corrupts the ready set.
        assert!(!on_wake(TaskState::Ready));
        assert!(!on_wake(TaskState::Running));
        assert!(!on_wake(TaskState::Exited));
    }

    /// A default-priority ready candidate — for runqueue tests that only care
    /// about ordering/membership of ids, not priority.
    fn rq(id: u32) -> Candidate {
        Candidate { id: TaskId(id), base: Priority::Normal, enqueued_tick: 0 }
    }

    #[test]
    fn new_runqueue_is_empty() {
        let q = Runqueue::new();
        assert!(q.is_empty());
        assert_eq!(q.len(), 0);
    }

    #[test]
    fn pop_from_empty_returns_none() {
        let mut q = Runqueue::new();
        assert!(q.pop_front().is_none());
    }

    #[test]
    fn remove_takes_a_specific_task_out_of_the_middle() {
        // Priority scheduling picks a task by effective priority, not FIFO
        // order, so the scheduler removes the chosen id wherever it sits.
        let mut q = Runqueue::new();
        q.push_back(rq(1));
        q.push_back(rq(2));
        q.push_back(rq(3));
        assert!(q.remove(TaskId(2)));
        assert_eq!(q.len(), 2);
        // The survivors keep their relative order.
        assert_eq!(q.pop_front().map(|c| c.id), Some(TaskId(1)));
        assert_eq!(q.pop_front().map(|c| c.id), Some(TaskId(3)));
    }

    #[test]
    fn remove_absent_task_is_a_noop_returning_false() {
        let mut q = Runqueue::new();
        q.push_back(rq(1));
        assert!(!q.remove(TaskId(9)));
        assert_eq!(q.len(), 1);
    }

    #[test]
    fn push_then_pop_returns_same_task() {
        let mut q = Runqueue::new();
        q.push_back(rq(7));
        assert!(!q.is_empty());
        assert_eq!(q.len(), 1);
        assert_eq!(q.pop_front().map(|c| c.id), Some(TaskId(7)));
        assert!(q.is_empty());
    }

    #[test]
    fn pop_order_is_fifo() {
        // Three tasks pushed in order pop in the same order — the default-all-
        // Normal case the priority pick still reduces to FIFO/longest-wait.
        let mut q = Runqueue::new();
        q.push_back(rq(1));
        q.push_back(rq(2));
        q.push_back(rq(3));
        assert_eq!(q.pop_front().map(|c| c.id), Some(TaskId(1)));
        assert_eq!(q.pop_front().map(|c| c.id), Some(TaskId(2)));
        assert_eq!(q.pop_front().map(|c| c.id), Some(TaskId(3)));
        assert!(q.pop_front().is_none());
    }

    #[test]
    fn pop_then_push_back_implements_round_robin_rotation() {
        // The scheduler's core idiom: pop the head, run it, push it
        // back at the tail. Repeating that should cycle through every
        // task in original order indefinitely.
        let mut q = Runqueue::new();
        q.push_back(rq(1));
        q.push_back(rq(2));
        q.push_back(rq(3));

        let first = q.pop_front().unwrap();
        q.push_back(first);
        let second = q.pop_front().unwrap();
        q.push_back(second);
        let third = q.pop_front().unwrap();
        q.push_back(third);

        // After three rotations, the head should be task 1 again.
        assert_eq!(q.pop_front().map(|c| c.id), Some(TaskId(1)));
    }

    #[test]
    fn len_tracks_pushes_and_pops() {
        let mut q = Runqueue::new();
        assert_eq!(q.len(), 0);
        q.push_back(rq(10));
        assert_eq!(q.len(), 1);
        q.push_back(rq(11));
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
        q.push_back(rq(0));
        q.push_back(rq(u32::MAX));
        assert_eq!(q.pop_front().map(|c| c.id), Some(TaskId(0)));
        assert_eq!(q.pop_front().map(|c| c.id), Some(TaskId(u32::MAX)));
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
        assert_eq!(pick_next(core::iter::empty(), 100, 10), None);
    }

    #[test]
    fn pick_single_candidate_returns_it() {
        assert_eq!(pick_next([cand(7, Priority::Low, 0)], 100, 10), Some(TaskId(7)));
    }

    #[test]
    fn pick_highest_base_priority_when_all_fresh() {
        // All enqueued "now" → no aging → pure base priority decides.
        let cs = [
            cand(1, Priority::Low, 100),
            cand(2, Priority::High, 100),
            cand(3, Priority::Normal, 100),
        ];
        assert_eq!(pick_next(cs, 100, 10), Some(TaskId(2)));
    }

    #[test]
    fn pick_aged_low_overtakes_fresh_normal() {
        // A Low task that has waited two steps ages to High and beats a freshly
        // enqueued Normal — this is the anti-starvation guarantee.
        let cs = [
            cand(1, Priority::Normal, 100), // fresh → effective Normal
            cand(2, Priority::Low, 80),     // waited 20 = 2 steps → effective High
        ];
        assert_eq!(pick_next(cs, 100, 10), Some(TaskId(2)));
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
        assert_eq!(pick_next(cs, 100, 1000), Some(TaskId(2)));
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
        assert_eq!(pick_next(cs, 100, 10), Some(TaskId(1)));
    }

    const HIGH: u8 = Priority::High as u8;
    const NORMAL: u8 = Priority::Normal as u8;

    #[test]
    fn no_preempt_when_nothing_is_ready() {
        // Quantum expired but no other task wants the CPU — keep running.
        assert!(!should_preempt(NORMAL, core::iter::empty(), 100, 10));
    }

    #[test]
    fn preempt_for_a_higher_priority_ready_task() {
        assert!(should_preempt(NORMAL, [cand(1, Priority::High, 100)], 100, 10));
    }

    #[test]
    fn preempt_for_an_equal_priority_ready_task() {
        // Same level → time-slice (round-robin within the level).
        assert!(should_preempt(NORMAL, [cand(1, Priority::Normal, 100)], 100, 10));
    }

    #[test]
    fn no_preempt_for_only_lower_priority_ready_tasks() {
        // The running task outranks everything ready — the timer must NOT
        // demote it across a level. This is the anti-inversion guarantee.
        assert!(!should_preempt(HIGH, [cand(1, Priority::Low, 100), cand(2, Priority::Normal, 100)], 100, 10));
    }

    #[test]
    fn preempt_once_a_low_task_has_aged_up_to_the_running_level() {
        // A Low task that has waited long enough to age to the running task's
        // level may preempt it — this is what lets aging rescue a starved task
        // even from a higher-priority CPU hog. Low base, waited 2 steps → High.
        assert!(should_preempt(HIGH, [cand(1, Priority::Low, 80)], 100, 10));
    }

    #[test]
    fn killing_an_off_cpu_live_target_terminates_it() {
        // Default = not self / not running-remote / not exited = an off-CPU live
        // target (`Ready` or `Blocked`). Both terminate: not running ⇒ stack + satp
        // quiescent, and any endpoint ghost is cleared by `cancel_wait` (inc 3.5).
        assert_eq!(classify_kill(KillTarget::default()), KillAction::Terminate);
    }

    #[test]
    fn killing_an_already_exited_task_is_a_no_op() {
        // Idempotent: a target that already exited (raced its own exit, or a
        // double-kill) succeeds without touching the scheduler again.
        assert_eq!(
            classify_kill(KillTarget { is_exited: true, ..Default::default() }),
            KillAction::NoOp
        );
    }

    #[test]
    fn killing_yourself_is_refused() {
        // Self-termination is `Exit`'s job, not `Kill`'s.
        assert_eq!(
            classify_kill(KillTarget { is_self: true, ..Default::default() }),
            KillAction::RefuseSelf
        );
    }

    #[test]
    fn killing_a_task_running_on_another_hart_is_deferred() {
        // A live target on another hart holds a loaded satp + an executing stack;
        // reaping it from here is a UAF. Deferred to v2b (needs an IPI to halt it).
        assert_eq!(
            classify_kill(KillTarget { running_remote: true, ..Default::default() }),
            KillAction::RefuseRunningRemote
        );
    }

    #[test]
    fn self_check_precedes_running_remote() {
        // Precedence: self is decided before the placement flags, so the running
        // task on this hart (which IS the caller) never routes to a kill path.
        assert_eq!(
            classify_kill(KillTarget { is_self: true, running_remote: true, is_exited: true }),
            KillAction::RefuseSelf
        );
    }
}
