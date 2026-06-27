//! Per-process span-name table — the span-naming security boundary.
//!
//! A process names its own spans. When it opens a span (the `SpanOpen` syscall),
//! the kernel resolves the name against *this process's own* table: a name the
//! process has used before resolves to the [`StringId`] it already leaked; a
//! genuinely new name (under the per-process quota) leaks a **fresh** id. There
//! is **no cross-process content dedup** — a process opening `"kernel.heartbeat"`
//! gets its own distinct id, never the kernel's, so it cannot emit a span under
//! another process's (or the kernel's) name, nor probe which names exist
//! system-wide by observing quota cost. This is the span twin of the
//! [`MetricTable`](super::metric::MetricTable) per-process scoping (debt #2);
//! before it, the `SpanOpen` path content-deduped against the global intern
//! table — the span-name poisoning + disclosure hole.
//!
//! The table is bounded by [`MAX_SPAN_NAMES`](SpanNameTable::MAX_SPAN_NAMES): the
//! capacity *is* the quota, bounding the permanent `Box::leak` *per process*.
//! Across process lifetimes the leak is **not** reclaimed today — and because
//! per-process scoping removed cross-process name dedup, each spawn re-leaks its
//! span names, so a long-running spawner (the v0.13 shell) grows it by
//! O(spawns × names-per-program). Accepted for now (Option A); reclaim-on-exit is
//! deferred to the v0.12 teardown milestone. See `plans/span-and-metric-name-gc.md`.
//!
//! Pure data + bookkeeping: no `unsafe`, no MMIO, no CSRs. Host-tested here; the
//! `kernel` side leaks the name into `'static`, allocates the global `StringId`
//! via the intern table, and stores the pair here.

use alloc::vec::Vec;

use protocol::StringId;

/// A process's span-name table: the `'static` span names it has introduced,
/// each paired with the global [`StringId`] it was interned to. Names are
/// append-only (no revocation) and bounded by
/// [`MAX_SPAN_NAMES`](Self::MAX_SPAN_NAMES) — the capacity *is* the per-process
/// quota. Lookups are by content (O(n), n ≤ cap), so a process's repeated open
/// of a name resolves to its own id without re-leaking, while a name another
/// process used is invisible here.
#[derive(Debug, Default)]
pub struct SpanNameTable {
    names: Vec<(&'static str, StringId)>,
}

impl SpanNameTable {
    /// Cap on distinct span names a single process may introduce. Mirrors the
    /// former `Process::MAX_SPAN_NAMES`: generous for a real program, small
    /// enough that a misbehaving one can't pin unbounded leaked names.
    pub const MAX_SPAN_NAMES: usize = 16;

    #[must_use]
    pub const fn new() -> Self {
        Self { names: Vec::new() }
    }

    /// The [`StringId`] this process already interned `name` to, by **content**,
    /// or `None` if it has not introduced that name. Content (not pointer)
    /// equality is what lets a runtime string copied from U-mode resolve to the
    /// process's own earlier leak.
    #[must_use]
    pub fn resolve(&self, name: &str) -> Option<StringId> {
        self.names
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, id)| *id)
    }

    /// Whether the table is at its [`MAX_SPAN_NAMES`](Self::MAX_SPAN_NAMES)
    /// quota — a further new name must be refused. The kernel checks this
    /// *before* leaking + interning, so a quota-refused open commits no name.
    #[must_use]
    pub fn is_full(&self) -> bool {
        self.names.len() >= Self::MAX_SPAN_NAMES
    }

    /// Record that this process interned `name` to `id`. The caller must have
    /// confirmed [`resolve`](Self::resolve) missed and the table is not
    /// [`is_full`](Self::is_full) — this is the bookkeeping half of a fresh
    /// registration.
    pub fn insert(&mut self, name: &'static str, id: StringId) {
        self.names.push((name, id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::StringId;
    extern crate std;
    use std::string::ToString;

    #[test]
    fn a_registered_name_resolves_to_its_string_id() {
        let mut table = SpanNameTable::new();
        table.insert("worker.tick", StringId(7));
        assert_eq!(table.resolve("worker.tick"), Some(StringId(7)));
    }

    #[test]
    fn an_unregistered_name_does_not_resolve() {
        let table = SpanNameTable::new();
        assert_eq!(table.resolve("never.opened"), None);
    }

    #[test]
    fn resolve_matches_on_content_not_pointer() {
        // The kernel resolves a runtime string copied from U-mode against the
        // stored `'static` names — so the match must be by value, letting a
        // process's repeat open of a name reuse its own id (no re-leak).
        let mut table = SpanNameTable::new();
        table.insert("worker.tick", StringId(3));
        let runtime_built = "worker.".to_string() + "tick";
        assert_eq!(table.resolve(&runtime_built), Some(StringId(3)));
    }

    #[test]
    fn distinct_names_resolve_to_their_own_ids() {
        let mut table = SpanNameTable::new();
        table.insert("a.tick", StringId(1));
        table.insert("b.tick", StringId(2));
        assert_eq!(table.resolve("a.tick"), Some(StringId(1)));
        assert_eq!(table.resolve("b.tick"), Some(StringId(2)));
        assert_eq!(table.resolve("c.tick"), None);
    }

    #[test]
    fn the_table_is_full_exactly_at_the_cap() {
        let mut table = SpanNameTable::new();
        assert!(!table.is_full(), "an empty table has room");
        for (i, &name) in NAMES.iter().enumerate() {
            assert!(!table.is_full(), "the table has room before insert {i}");
            table.insert(name, StringId(i as u32));
        }
        assert!(table.is_full(), "the table is full at the cap");
    }

    /// Sixteen distinct `'static` names — one per quota slot — for the
    /// fill-to-cap test. (The kernel leaks names into `'static`; tests use
    /// literals, which already are.)
    const NAMES: [&str; SpanNameTable::MAX_SPAN_NAMES] = [
        "s00", "s01", "s02", "s03", "s04", "s05", "s06", "s07", "s08", "s09", "s10", "s11", "s12",
        "s13", "s14", "s15",
    ];
}
