//! Span parent-stack bookkeeping. Two pieces:
//!
//! - [`SpanIds`] — global monotonic id allocator. One static for the
//!   whole kernel; span ids are unique across all tasks so wire frames
//!   and Tempo trace IDs don't collide.
//! - [`SpanCursor`] — per-task tracker of the innermost open span.
//!   Lives inside each `Task`; the scheduler swaps which cursor the
//!   span open/close paths consult on context switch.
//!
//! No frame emission here — the kernel binary owns the wire
//! (`SpanStart` at open, `SpanEnd` at close, both with timestamps from
//! a `Clock`). Keeping emission out of this module means tests can run
//! without a sink or a clock.

use core::sync::atomic::{AtomicU64, Ordering};

use protocol::SpanId;

/// Global span-id allocator. One static instance for the whole kernel.
/// `AtomicU64` because multiple harts (eventually) and multiple tasks
/// (today, after v0.5) call `allocate` concurrently.
pub struct SpanIds {
    next_id: AtomicU64,
}

impl SpanIds {
    /// Construct with id allocator at 1 — `SpanId(0)` is reserved as
    /// the "no parent" / root sentinel.
    pub const fn new() -> Self {
        Self { next_id: AtomicU64::new(1) }
    }

    /// Hand out the next span id. `Relaxed`: the atomic *is* the id
    /// allocation; no other memory synchronises through it. Multi-hart
    /// contention on this single counter is a known scaling corner —
    /// per-CPU partitioning is documented in `plans/scaling-corners.md`.
    pub fn allocate(&self) -> SpanId {
        SpanId(self.next_id.fetch_add(1, Ordering::Relaxed))
    }
}

impl Default for SpanIds {
    fn default() -> Self {
        Self::new()
    }
}

/// Per-task tracker of "the innermost open span on this task." Loaded
/// to get the parent when opening a new span; stored to install or
/// restore on open / close.
///
/// `AtomicU64` rather than a plain `u64` because the field is accessed
/// through `&self`. Cooperative-single-task today, but the API shape
/// supports any access pattern (preempt mid-span, IRQ handler opening
/// its own span on the same hart, etc.) without changing callers.
///
/// `Relaxed` everywhere: each cursor is owned by exactly one task,
/// and at most one task runs per hart at a time, so accesses are
/// effectively per-CPU. No cross-hart observer means no ordering
/// needed.
pub struct SpanCursor {
    current: AtomicU64,
}

impl SpanCursor {
    pub const fn new() -> Self {
        Self { current: AtomicU64::new(0) }
    }

    /// Current innermost span id, or `SpanId(0)` if no span is open.
    pub fn current(&self) -> SpanId {
        SpanId(self.current.load(Ordering::Relaxed))
    }

    /// Seed the innermost span directly — used to install *incoming* trace
    /// context (e.g. the sender's span arriving over IPC) so the next [`open`]
    /// on this cursor makes its span a child of `span`. Distinct from `open`,
    /// which mints a fresh id; this installs an id minted elsewhere.
    pub fn set_current(&self, span: SpanId) {
        self.current.store(span.0, Ordering::Relaxed);
    }
}

impl Default for SpanCursor {
    fn default() -> Self {
        Self::new()
    }
}

/// Bookkeeping result from `open`. The caller emits `SpanStart` using
/// these fields, then stashes the value so it can hand it back to
/// `close` when the span ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpanOpen {
    pub id: SpanId,
    pub parent: SpanId,
}

/// Open a new span. Mints a fresh id from `ids`, records the previous
/// innermost as the parent, installs the new id as innermost on
/// `cursor`.
pub fn open(ids: &SpanIds, cursor: &SpanCursor) -> SpanOpen {
    let parent = SpanId(cursor.current.load(Ordering::Relaxed));
    let id = ids.allocate();
    cursor.current.store(id.0, Ordering::Relaxed);
    SpanOpen { id, parent }
}

/// Close the span whose `open` returned `span`. Restores `cursor` to
/// that span's parent.
pub fn close(cursor: &SpanCursor, span: &SpanOpen) {
    cursor.current.store(span.parent.0, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_span_has_no_parent() {
        let ids = SpanIds::new();
        let cursor = SpanCursor::new();
        let s = open(&ids, &cursor);
        assert_eq!(s.parent, SpanId(0));
    }

    #[test]
    fn set_current_seeds_the_parent_for_the_next_open() {
        // Seeding an incoming span id (e.g. trace context arriving over IPC)
        // makes the next span opened on this cursor a child of it.
        let ids = SpanIds::new();
        let cursor = SpanCursor::new();
        cursor.set_current(SpanId(99));
        let s = open(&ids, &cursor);
        assert_eq!(s.parent, SpanId(99));
    }

    #[test]
    fn first_id_is_one() {
        // Reserve 0 for "no parent" / sentinel — so the first allocated
        // span must be 1, not 0.
        let ids = SpanIds::new();
        let cursor = SpanCursor::new();
        let s = open(&ids, &cursor);
        assert_eq!(s.id, SpanId(1));
    }

    #[test]
    fn nested_open_records_outer_as_parent() {
        let ids = SpanIds::new();
        let cursor = SpanCursor::new();
        let outer = open(&ids, &cursor);
        let inner = open(&ids, &cursor);
        assert_eq!(inner.parent, outer.id);
    }

    #[test]
    fn close_restores_cursor_to_parent() {
        let ids = SpanIds::new();
        let cursor = SpanCursor::new();
        let outer = open(&ids, &cursor);
        let inner = open(&ids, &cursor);
        assert_eq!(cursor.current(), inner.id);
        close(&cursor, &inner);
        assert_eq!(cursor.current(), outer.id);
        close(&cursor, &outer);
        assert_eq!(cursor.current(), SpanId(0));
    }

    #[test]
    fn siblings_share_parent_and_have_distinct_ids() {
        let ids = SpanIds::new();
        let cursor = SpanCursor::new();
        let outer = open(&ids, &cursor);
        let a = open(&ids, &cursor);
        close(&cursor, &a);
        let b = open(&ids, &cursor);
        assert_eq!(a.parent, outer.id);
        assert_eq!(b.parent, outer.id);
        assert_ne!(a.id, b.id);
    }

    #[test]
    fn ids_monotonically_increase_across_open_close() {
        // Closing a span doesn't recycle its id — important for the
        // host-side decoder to disambiguate spans.
        let ids = SpanIds::new();
        let cursor = SpanCursor::new();
        let a = open(&ids, &cursor);
        close(&cursor, &a);
        let b = open(&ids, &cursor);
        assert_eq!(a.id, SpanId(1));
        assert_eq!(b.id, SpanId(2));
    }

    #[test]
    fn two_cursors_with_shared_ids_dont_share_current_span() {
        // The whole point of splitting: two tasks each have their own
        // cursor, but draw from the same global id pool. Span ids stay
        // unique; "innermost open span" is per-task.
        let ids = SpanIds::new();
        let cursor_a = SpanCursor::new();
        let cursor_b = SpanCursor::new();

        let a_outer = open(&ids, &cursor_a);
        let b_outer = open(&ids, &cursor_b);

        // Each task sees its own current; ids are globally unique.
        assert_eq!(cursor_a.current(), a_outer.id);
        assert_eq!(cursor_b.current(), b_outer.id);
        assert_ne!(a_outer.id, b_outer.id);

        // B's outer is NOT A's parent. If we'd kept the global cursor,
        // a's outer would have B's id as parent — that's exactly the
        // bug the split avoids.
        assert_eq!(a_outer.parent, SpanId(0));
        assert_eq!(b_outer.parent, SpanId(0));
    }
}
