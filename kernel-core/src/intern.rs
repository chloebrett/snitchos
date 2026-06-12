//! Intern table for string names that appear in the wire protocol.
//! Pointer-keyed (not content-keyed) — see `lookup_or_insert` for why.
//! Frame emission for newly-registered names is delegated to a
//! `FrameSink` so the kernel and tests share one code path.

use protocol::{Frame, MetricKind, StringId};

use crate::sink::FrameSink;

/// Maximum number of unique strings the table can hold. Each task
/// `spawn` adds ~3 strings (the task name + 2 per-task metric names).
/// Bumped to 128 in v0.6 step 10 because adding `hart_1_main` +
/// `hart_1_probe` to the existing v0.5 task set + workload metrics +
/// SMP metrics pushed past 64.
pub const MAX_INTERNED: usize = 128;

#[derive(Copy, Clone)]
struct InternEntry {
    name: &'static str,
    metric_registered: bool,
}

pub struct InternTable {
    entries: [Option<InternEntry>; MAX_INTERNED],
    next_id: u32,
}

impl Default for InternTable {
    fn default() -> Self {
        Self::new()
    }
}

impl InternTable {
    pub const fn new() -> Self {
        Self {
            entries: [None; MAX_INTERNED],
            next_id: 0,
        }
    }

    /// Scan + insert. Returns `(id, is_new)` so callers know whether to
    /// emit a `StringRegister`. Equality is by **pointer**, not value —
    /// two `&'static str`s with the same characters from different
    /// allocations get distinct ids. Acceptable for v0.1 (one crate
    /// minting names); revisit if userspace ever registers names.
    ///
    /// Panics if the table is full. Programmer error: bump
    /// `MAX_INTERNED` or stop creating unique names.
    fn lookup_or_insert(&mut self, name: &'static str) -> (StringId, bool) {
        for (i, entry) in self.entries.iter().enumerate() {
            if let Some(e) = entry
                && e.name.as_ptr() == name.as_ptr() {
                    return (StringId(i as u32), false);
                }
        }

        let id = self.next_id;
        let slot = id as usize;
        assert!(slot < MAX_INTERNED, 
            "intern table full ({MAX_INTERNED} entries); bump MAX_INTERNED",
        );
        self.entries[slot] = Some(InternEntry {
            name,
            metric_registered: false,
        });
        self.next_id = id + 1;
        (StringId(id), true)
    }

    /// Look up `name`, allocating a slot + emitting `StringRegister`
    /// if it's new. Generic over `S` (not `&mut dyn FrameSink`) so the
    /// compiler monomorphizes per-call — no vtable, no absolute
    /// function pointers stored in static memory. Important for the
    /// kernel's higher-half link layout: dyn dispatch would compile
    /// to absolute function pointer loads that wouldn't resolve
    /// until paging is on with a higher-half mapping.
    pub fn register_or_lookup<S: FrameSink>(
        &mut self,
        name: &'static str,
        sink: &mut S,
    ) -> StringId {
        let (id, is_new) = self.lookup_or_insert(name);
        if is_new {
            sink.emit(&Frame::StringRegister { id, value: name });
        }
        id
    }

    /// Find `name` by **content** (not pointer), returning its id if some
    /// same-valued name is already registered. Unlike [`register_or_lookup`]'s
    /// pointer equality, this lets a runtime-built string — e.g. a userspace
    /// span name copied in per-syscall — resolve to an existing id, so the
    /// caller leaks-and-registers only on a genuine first sighting rather than
    /// on every repeated call (which would overflow the table). O(n) over the
    /// table; the table is small.
    #[must_use]
    pub fn lookup_by_content(&self, name: &str) -> Option<StringId> {
        for (i, entry) in self.entries.iter().enumerate() {
            if let Some(e) = entry
                && e.name == name
            {
                return Some(StringId(i as u32));
            }
        }
        None
    }

    /// Register `name` as a metric with `kind`. Emits `StringRegister`
    /// if the name is new, then `MetricRegister` if the name wasn't
    /// previously declared as a metric. Calling twice with different
    /// kinds is a programmer error — the second call sees
    /// `metric_registered: true` and skips emit, so the host's first-seen
    /// kind wins.
    pub fn register_metric<S: FrameSink>(
        &mut self,
        name: &'static str,
        kind: MetricKind,
        sink: &mut S,
    ) -> StringId {
        let (id, is_new) = self.lookup_or_insert(name);
        if is_new {
            sink.emit(&Frame::StringRegister { id, value: name });
        }
        let entry = self.entries[id.0 as usize]
            .as_mut()
            .expect("lookup_or_insert guarantees the slot is populated");
        if !entry.metric_registered {
            entry.metric_registered = true;
            sink.emit(&Frame::MetricRegister { name_id: id, kind });
        }
        id
    }

    /// Number of distinct names currently held.
    pub fn count(&self) -> u32 {
        self.next_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sink::capture::CapturingSink;
    extern crate std;
    use std::boxed::Box;
    use std::format;

    fn decode(bytes: &[u8]) -> Frame<'_> {
        postcard::from_bytes(bytes).unwrap()
    }

    #[test]
    fn new_name_assigns_id_zero_and_emits_string_register() {
        let mut table = InternTable::new();
        let mut sink = CapturingSink::new();

        let id = table.register_or_lookup("foo", &mut sink);

        assert_eq!(id, StringId(0));
        assert_eq!(sink.len(), 1);
        assert_eq!(
            decode(&sink.raw()[0]),
            Frame::StringRegister { id: StringId(0), value: "foo" },
        );
    }

    #[test]
    fn same_pointer_returns_same_id_without_re_emitting() {
        let mut table = InternTable::new();
        let mut sink = CapturingSink::new();

        let id1 = table.register_or_lookup("foo", &mut sink);
        let id2 = table.register_or_lookup("foo", &mut sink);

        assert_eq!(id1, id2);
        assert_eq!(sink.len(), 1, "second lookup must not emit");
    }

    #[test]
    fn lookup_by_content_finds_a_registered_name_by_value() {
        let mut table = InternTable::new();
        let mut sink = CapturingSink::new();
        let id = table.register_or_lookup("worker.tick", &mut sink);
        assert_eq!(table.lookup_by_content("worker.tick"), Some(id));
    }

    #[test]
    fn lookup_by_content_returns_none_for_an_unregistered_name() {
        let table = InternTable::new();
        assert_eq!(table.lookup_by_content("never.registered"), None);
    }

    #[test]
    fn lookup_by_content_matches_on_value_not_pointer() {
        // Unlike `register_or_lookup` (pointer equality), content lookup
        // resolves a runtime-built string to the id of a same-valued
        // registered name — the property userspace span names rely on so that
        // repeating a name doesn't re-register and overflow the table.
        let mut table = InternTable::new();
        let mut sink = CapturingSink::new();
        let registered: &'static str = Box::leak(Box::<str>::from("dup"));
        let id = table.register_or_lookup(registered, &mut sink);
        let runtime_built = format!("d{}", "up"); // distinct allocation, same chars
        assert_eq!(table.lookup_by_content(&runtime_built), Some(id));
    }

    #[test]
    fn same_content_different_pointer_gets_distinct_ids() {
        // Documents the pointer-equality choice. Two different
        // `&'static str` allocations with identical content.
        let a: &'static str = Box::leak(Box::<str>::from("dup"));
        let b: &'static str = Box::leak(Box::<str>::from("dup"));
        assert_ne!(a.as_ptr(), b.as_ptr(), "leaked strs must have distinct pointers");

        let mut table = InternTable::new();
        let mut sink = CapturingSink::new();

        let id_a = table.register_or_lookup(a, &mut sink);
        let id_b = table.register_or_lookup(b, &mut sink);

        assert_ne!(id_a, id_b);
        assert_eq!(sink.len(), 2);
    }

    #[test]
    #[should_panic(expected = "intern table full")]
    fn filling_to_max_then_one_more_panics() {
        let mut table = InternTable::new();
        let mut sink = CapturingSink::new();
        for i in 0..MAX_INTERNED {
            let s: &'static str = Box::leak(format!("name{i}").into_boxed_str());
            table.register_or_lookup(s, &mut sink);
        }
        let overflow: &'static str = Box::leak(Box::<str>::from("one too many"));
        table.register_or_lookup(overflow, &mut sink);
    }

    #[test]
    fn register_counter_then_gauge_does_not_redeclare_kind() {
        // Programmer error mode, documented in the source: the second
        // call sees `metric_registered: true` and skips emit. Host
        // never learns the new kind. Test pins this so we don't
        // silently "fix" it without an explicit decision.
        let mut table = InternTable::new();
        let mut sink = CapturingSink::new();

        table.register_metric("m", MetricKind::Counter, &mut sink);
        table.register_metric("m", MetricKind::Gauge, &mut sink);

        // Exactly: StringRegister + MetricRegister(Counter). Nothing else.
        assert_eq!(sink.len(), 2);
        assert!(matches!(
            decode(&sink.raw()[1]),
            Frame::MetricRegister { kind: MetricKind::Counter, .. },
        ));
    }

    #[test]
    fn lookup_then_register_metric_emits_metric_register_only() {
        // Name was interned for a span first; later promoted to a metric.
        // StringRegister fires on first call, MetricRegister on second
        // — no duplicate StringRegister.
        let mut table = InternTable::new();
        let mut sink = CapturingSink::new();

        table.register_or_lookup("m", &mut sink);
        table.register_metric("m", MetricKind::Counter, &mut sink);

        assert_eq!(sink.len(), 2);
        assert!(matches!(decode(&sink.raw()[0]), Frame::StringRegister { .. }));
        assert!(matches!(decode(&sink.raw()[1]), Frame::MetricRegister { .. }));
    }

    #[test]
    fn count_reflects_distinct_names() {
        let mut table = InternTable::new();
        let mut sink = CapturingSink::new();
        assert_eq!(table.count(), 0);
        table.register_or_lookup("a", &mut sink);
        table.register_or_lookup("b", &mut sink);
        table.register_or_lookup("a", &mut sink); // duplicate, no bump
        assert_eq!(table.count(), 2);
    }
}
