//! Span parent-stack bookkeeping. Pure: allocates monotonically-
//! increasing span ids and tracks which span is currently innermost,
//! so a new span can record its parent and Drop can restore it.
//!
//! No frame emission here — the kernel binary owns the wire
//! (`SpanStart` at open, `SpanEnd` at close, both with timestamps from
//! a `Clock`). Keeping emission out of this module means tests can run
//! without a sink or a clock, and the kernel doesn't have to thread a
//! sink through `Drop`.

use core::sync::atomic::{AtomicU64, Ordering};

use protocol::SpanId;

/// Per-hart span bookkeeping. Single-hart for v0.1; SMP will need one
/// of these per CPU (and partitioned id space — see the plan).
pub struct SpanRegistry {
    /// Next id to hand out. Starts at 1 so `SpanId(0)` reads as "no
    /// parent" / root.
    next_id: AtomicU64,
    /// Innermost open span on this hart, or 0 if none open.
    current: AtomicU64,
}

/// Bookkeeping result from `open`. The caller emits `SpanStart` using
/// these fields, then stashes `id` + `parent` so it can hand them back
/// to `close` when the span ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpanOpen {
    pub id: SpanId,
    pub parent: SpanId,
}

impl SpanRegistry {
    pub const fn new() -> Self {
        Self {
            next_id: AtomicU64::new(1),
            current: AtomicU64::new(0),
        }
    }

    /// Open a new span. Mints a fresh id, records the previously-current
    /// span as the parent, and installs the new span as current.
    pub fn open(&self) -> SpanOpen {
        let parent = SpanId(self.current.load(Ordering::Relaxed));
        let id = SpanId(self.next_id.fetch_add(1, Ordering::Relaxed));
        self.current.store(id.0, Ordering::Relaxed);
        SpanOpen { id, parent }
    }

    /// Close the span whose `open` returned `span`. Restores `current`
    /// to that span's parent.
    pub fn close(&self, span: &SpanOpen) {
        self.current.store(span.parent.0, Ordering::Relaxed);
    }

    /// Current innermost span id, or `SpanId(0)` if none open. Exposed
    /// for tests; not used by the kernel today.
    pub fn current(&self) -> SpanId {
        SpanId(self.current.load(Ordering::Relaxed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_span_has_no_parent() {
        let reg = SpanRegistry::new();
        let s = reg.open();
        assert_eq!(s.parent, SpanId(0));
    }

    #[test]
    fn first_id_is_one() {
        // Reserve 0 for "no parent" / sentinel — so the first allocated
        // span must be 1, not 0.
        let reg = SpanRegistry::new();
        let s = reg.open();
        assert_eq!(s.id, SpanId(1));
    }

    #[test]
    fn nested_open_records_outer_as_parent() {
        let reg = SpanRegistry::new();
        let outer = reg.open();
        let inner = reg.open();
        assert_eq!(inner.parent, outer.id);
    }

    #[test]
    fn close_restores_current_to_parent() {
        let reg = SpanRegistry::new();
        let outer = reg.open();
        let inner = reg.open();
        assert_eq!(reg.current(), inner.id);
        reg.close(&inner);
        assert_eq!(reg.current(), outer.id);
        reg.close(&outer);
        assert_eq!(reg.current(), SpanId(0));
    }

    #[test]
    fn siblings_share_parent_and_have_distinct_ids() {
        let reg = SpanRegistry::new();
        let outer = reg.open();
        let a = reg.open();
        reg.close(&a);
        let b = reg.open();
        assert_eq!(a.parent, outer.id);
        assert_eq!(b.parent, outer.id);
        assert_ne!(a.id, b.id);
    }

    #[test]
    fn ids_monotonically_increase_across_open_close() {
        // Closing a span doesn't recycle its id — important for the
        // host-side decoder to disambiguate spans.
        let reg = SpanRegistry::new();
        let a = reg.open();
        reg.close(&a);
        let b = reg.open();
        assert_eq!(a.id, SpanId(1));
        assert_eq!(b.id, SpanId(2));
    }
}
