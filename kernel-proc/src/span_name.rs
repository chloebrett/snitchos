//! Per-process span-name table — the span-naming security boundary.
//!
//! A process names its own spans. When it opens a span (the `SpanOpen` syscall),
//! the kernel resolves the name against *this process's own* table: a name the
//! process has used before resolves to the [`StringId`] it already registered; a
//! genuinely new name (under the per-process quota) registers a **fresh** id. There
//! is **no cross-process content dedup** — a process opening `"kernel.heartbeat"`
//! gets its own distinct id, never the kernel's, so it cannot emit a span under
//! another process's (or the kernel's) name, nor probe which names exist
//! system-wide by observing quota cost. This is the span twin of the
//! [`MetricTable`](super::metric::MetricTable) per-process scoping (debt #2);
//! before it, the `SpanOpen` path content-deduped against the global intern
//! table — the span-name poisoning + disclosure hole.
//!
//! The table is bounded by [`MAX_SPAN_NAMES`](SpanNameTable::MAX_SPAN_NAMES): the
//! capacity *is* the per-process quota. The names are **owned** (`Box<str>`) and
//! **reclaimed on process exit** — `reap_task` walks [`ids`](SpanNameTable::ids)
//! and releases each from the global intern table, then this table is dropped.
//! (Per-process scoping removed cross-process dedup, so each spawn registers its
//! own names; without reclaim a long-running spawner like the shell would grow the
//! intern table by O(spawns × names). See `plans/legacy/span-and-metric-name-gc.md`.)
//!
//! Pure data + bookkeeping: no `unsafe`, no MMIO, no CSRs. Host-tested here; the
//! `kernel` side allocates the global `StringId` via the intern table (which owns
//! its own copy of the name under that id) and stores `(name, id)` here.

use alloc::boxed::Box;
use alloc::vec::Vec;

use protocol::StringId;

/// A process's span-name table: the owned span names it has introduced, each
/// paired with the global [`StringId`] it was interned to. Names are append-only
/// for the process's lifetime (no revocation) and bounded by
/// [`MAX_SPAN_NAMES`](Self::MAX_SPAN_NAMES) — the capacity *is* the per-process
/// quota. Lookups are by content (O(n), n ≤ cap), so a process's repeated open
/// of a name resolves to its own id without re-registering, while a name another
/// process used is invisible here. Dropped (and its ids released) on exit.
#[derive(Debug, Default)]
pub struct SpanNameTable {
    names: Vec<(Box<str>, StringId)>,
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
            .find(|(n, _)| n.as_ref() == name)
            .map(|(_, id)| *id)
    }

    /// Whether the table is at its [`MAX_SPAN_NAMES`](Self::MAX_SPAN_NAMES)
    /// quota — a further new name must be refused. The kernel checks this
    /// *before* leaking + interning, so a quota-refused open commits no name.
    #[must_use]
    pub fn is_full(&self) -> bool {
        self.names.len() >= Self::MAX_SPAN_NAMES
    }

    /// Record that this process interned `name` to `id`, taking **ownership** of
    /// the name so it can be dropped on process exit (the intern table owns its
    /// own copy under the same `id`; both are reclaimed on teardown). The caller
    /// must have confirmed [`resolve`](Self::resolve) missed and the table is not
    /// [`is_full`](Self::is_full) — this is the bookkeeping half of a fresh
    /// registration.
    pub fn insert(&mut self, name: Box<str>, id: StringId) {
        self.names.push((name, id));
    }

    /// Every [`StringId`] this process interned as a span name, for exit-time
    /// reclaim: the kernel releases each from the intern table when the process
    /// is reaped.
    pub fn ids(&self) -> impl Iterator<Item = StringId> + '_ {
        self.names.iter().map(|(_, id)| *id)
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
        table.insert(Box::from("worker.tick"), StringId(7));
        assert_eq!(table.resolve("worker.tick"), Some(StringId(7)));
    }

    #[test]
    fn an_unregistered_name_does_not_resolve() {
        let table = SpanNameTable::new();
        assert_eq!(table.resolve("never.opened"), None);
    }

    #[test]
    fn ids_lists_every_interned_string_id_for_teardown() {
        // On process exit the kernel walks these ids and releases each from the
        // intern table, reclaiming the per-process span names.
        let mut table = SpanNameTable::new();
        table.insert(Box::from("a.tick"), StringId(3));
        table.insert(Box::from("b.tick"), StringId(8));
        assert_eq!(table.ids().collect::<std::vec::Vec<_>>(), std::vec![StringId(3), StringId(8)]);
    }

    #[test]
    fn resolve_matches_on_content_not_pointer() {
        // The kernel resolves a runtime string copied from U-mode against the
        // stored `'static` names — so the match must be by value, letting a
        // process's repeat open of a name reuse its own id (no re-leak).
        let mut table = SpanNameTable::new();
        table.insert(Box::from("worker.tick"), StringId(3));
        let runtime_built = "worker.".to_string() + "tick";
        assert_eq!(table.resolve(&runtime_built), Some(StringId(3)));
    }

    #[test]
    fn distinct_names_resolve_to_their_own_ids() {
        let mut table = SpanNameTable::new();
        table.insert(Box::from("a.tick"), StringId(1));
        table.insert(Box::from("b.tick"), StringId(2));
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
            table.insert(Box::from(name), StringId(i as u32));
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
