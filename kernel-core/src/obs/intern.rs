//! Intern table for string names that appear in the wire protocol.
//! Pointer-keyed (not content-keyed) — see `lookup_or_insert` for why.
//! Frame emission for newly-registered names is delegated to a
//! `FrameSink` so the kernel and tests share one code path.

use alloc::boxed::Box;
use alloc::vec::Vec;

use protocol::{Frame, MetricKind, StringId};

use crate::sink::FrameSink;

/// Inline capacity: how many strings the table holds without touching the
/// allocator. The first strings are interned during boot *before the
/// allocator exists* — `span!("kernel.boot")` runs long before `heap::init()`
/// (see `kmain`) — so that prefix must live in fixed, allocator-free storage.
/// Everything registered past this spills into the heap-backed `overflow`,
/// which is only ever pushed once the heap is up.
///
/// **Invariant:** the number of strings interned *before* `heap::init` must
/// not exceed `INLINE_CAP` — a spill before the allocator exists would
/// `Vec::push` with no heap and fault. Pre-heap interning is just
/// `kernel.boot` + the init-phase spans (~10-20), so 64 is comfortable
/// headroom. (This is the one boot-order coupling the table carries.)
///
/// **Why O(n) lookup is fine:** every lookup is a linear scan, and the total
/// `n` is bounded in practice — kernel strings are a known, modest set (~100),
/// and the *userspace* contribution is bounded by a per-process span-name
/// quota (`handle_span_open`), so a program cannot drive `n` to where the scan
/// — or the allocation of each new name — becomes a denial of service. A few
/// hundred short `memcmp`s per lookup is tens of microseconds: not worth a
/// hash index. (If thousands of names ever become real, add a
/// `BTreeMap<&str, StringId>` index then.) Userspace names are `Owned` and
/// reclaimed on process exit via [`InternTable::release`]; kernel `&'static`
/// literals are a bounded, permanent set.
pub const INLINE_CAP: usize = 64;

/// An interned name. Kernel literals (`span!("kernel.boot")`, the `snitchos.*`
/// metrics) are a bounded, permanent set held as borrowed `&'static`; userspace
/// names are heap-`Owned` so they can be dropped when their process exits.
enum InternName {
    Static(&'static str),
    Owned(Box<str>),
}

impl InternName {
    fn as_str(&self) -> &str {
        match self {
            InternName::Static(s) => s,
            InternName::Owned(b) => b,
        }
    }
}

struct InternEntry {
    name: InternName,
    metric_registered: bool,
}

/// Two-region intern storage: a fixed inline array for the pre-allocator boot
/// prefix, then a heap-backed `Vec` for everything after. Ids are dense and
/// contiguous across the boundary — `0..INLINE_CAP` live inline, `INLINE_CAP..`
/// live in `overflow` at index `id - INLINE_CAP`.
pub struct InternTable {
    inline: [Option<InternEntry>; INLINE_CAP],
    inline_len: usize,
    overflow: Vec<Option<InternEntry>>,
}

impl Default for InternTable {
    fn default() -> Self {
        Self::new()
    }
}

impl InternTable {
    pub const fn new() -> Self {
        Self {
            // `InternEntry` is no longer `Copy` (it can own a `Box<str>`), so the
            // `[None; N]` repeat-init won't elaborate; the inline-const form does.
            inline: [const { None }; INLINE_CAP],
            inline_len: 0,
            overflow: Vec::new(),
        }
    }

    /// Walk every live entry with its dense id, inline region first.
    fn iter(&self) -> impl Iterator<Item = (u32, &InternEntry)> {
        self.inline[..self.inline_len]
            .iter()
            .flatten()
            .enumerate()
            .map(|(i, e)| (i as u32, e))
            .chain(
                self.overflow
                    .iter()
                    .enumerate()
                    .filter_map(|(i, e)| e.as_ref().map(|e| ((INLINE_CAP + i) as u32, e))),
            )
    }

    /// Mutable access to the entry with `id`, across the region boundary.
    /// `None` for a tombstoned (released) slot as well as an out-of-range id.
    fn entry_mut(&mut self, id: usize) -> Option<&mut InternEntry> {
        if id < INLINE_CAP {
            self.inline[id].as_mut()
        } else {
            self.overflow.get_mut(id - INLINE_CAP).and_then(Option::as_mut)
        }
    }

    /// Append a fresh entry, returning its dense id. Fills the inline region
    /// first; once full, spills into the heap-backed `overflow` (which is why
    /// the pre-heap string count must stay under `INLINE_CAP`).
    fn push(&mut self, entry: InternEntry) -> u32 {
        if self.inline_len < INLINE_CAP {
            let id = self.inline_len;
            self.inline[id] = Some(entry);
            self.inline_len += 1;
            id as u32
        } else {
            let id = INLINE_CAP + self.overflow.len();
            self.overflow.push(Some(entry));
            id as u32
        }
    }

    /// The metric twin of [`register_owned`](Self::register_owned): intern a
    /// heap-`Owned` metric name (the `RegisterMetric` syscall path), emitting its
    /// `StringRegister` then a `MetricRegister` carrying `kind` and the registering
    /// `task_id` (the emitter dimension). Always appends — no cross-process dedup,
    /// so each process's metric is its own `StringId` and can't forge another's.
    /// The name is reclaimable via [`release`](Self::release) on process exit.
    pub fn register_metric_owned<S: FrameSink>(
        &mut self,
        name: Box<str>,
        kind: MetricKind,
        task_id: u32,
        sink: &mut S,
    ) -> StringId {
        let id = self.push(InternEntry {
            name: InternName::Owned(name),
            metric_registered: true,
        });
        let entry = self
            .entry_mut(id as usize)
            .expect("the entry was just pushed");
        sink.emit(&Frame::StringRegister {
            id: StringId(id),
            value: entry.name.as_str(),
        });
        sink.emit(&Frame::MetricRegister { name_id: StringId(id), kind, task_id });
        StringId(id)
    }

    /// Reclaim the name at `id`, dropping its owned bytes and **tombstoning** the
    /// slot. The id is never reused: `push` only ever appends past the high-water
    /// mark, so a freed id can't be re-minted to alias a different name (the wire
    /// identity the collector keys its id→name map on). A released id resolves to
    /// nothing and stops appearing in new frames. Out-of-range / already-released
    /// ids are no-ops. Intended for `Owned` (per-process) names on process exit —
    /// kernel `&'static` literals are permanent and never released.
    pub fn release(&mut self, id: StringId) {
        let i = id.0 as usize;
        if i < INLINE_CAP {
            self.inline[i] = None;
        } else if let Some(slot) = self.overflow.get_mut(i - INLINE_CAP) {
            *slot = None;
        }
    }

    /// Scan + insert. Returns `(id, is_new)` so callers know whether to
    /// emit a `StringRegister`. Equality is by **pointer**, not value —
    /// two `&'static str`s with the same characters from different
    /// allocations get distinct ids. The content-keyed [`lookup_by_content`]
    /// is the path for runtime-built names (e.g. userspace span names).
    fn lookup_or_insert(&mut self, name: &'static str) -> (StringId, bool) {
        for (id, e) in self.iter() {
            if e.name.as_str().as_ptr() == name.as_ptr() {
                return (StringId(id), false);
            }
        }
        let id = self.push(InternEntry {
            name: InternName::Static(name),
            metric_registered: false,
        });
        (StringId(id), true)
    }

    /// Intern a heap-`Owned` name (a userspace span/metric name), emitting its
    /// `StringRegister`, and return its id. Unlike [`register_or_lookup`] this
    /// **always appends** — the caller (a per-process `SpanNameTable` /
    /// `MetricTable`) has already deduped within its own scope, and the table
    /// must *not* dedup across processes (that's the name-poisoning boundary).
    /// The table takes ownership so the name can be dropped via [`release`] when
    /// the process exits.
    pub fn register_owned<S: FrameSink>(&mut self, name: Box<str>, sink: &mut S) -> StringId {
        let id = self.push(InternEntry {
            name: InternName::Owned(name),
            metric_registered: false,
        });
        let entry = self
            .entry_mut(id as usize)
            .expect("the entry was just pushed");
        sink.emit(&Frame::StringRegister {
            id: StringId(id),
            value: entry.name.as_str(),
        });
        StringId(id)
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
    /// same-valued name is already registered. O(n) over the table; the table is
    /// small.
    ///
    /// **Do NOT use this for per-process name resolution** (span or metric
    /// names). It deduplicates across the *whole* table — every process and the
    /// kernel — so resolving a userspace name through it would let a process
    /// alias another's (or the kernel's) `StringId`: the span-name poisoning hole
    /// fixed by per-process [`SpanNameTable`](crate::span_name::SpanNameTable) /
    /// [`MetricTable`](crate::metric::MetricTable) scoping. Kept for content
    /// lookups that are legitimately global (and as a test oracle for the
    /// inline→overflow scan).
    #[must_use]
    pub fn lookup_by_content(&self, name: &str) -> Option<StringId> {
        for (id, e) in self.iter() {
            if e.name.as_str() == name {
                return Some(StringId(id));
            }
        }
        None
    }

    /// Register `name` as a metric with `kind`, registered by `task_id` (the
    /// emitter dimension — `protocol::NO_EMITTER` for a kernel-global metric).
    /// Emits `StringRegister` if the name is new, then `MetricRegister` (carrying
    /// the kind + emitter) if the name wasn't previously declared as a metric.
    /// Calling twice with different kinds is a programmer error — the second call
    /// sees `metric_registered: true` and skips emit, so the host's first-seen
    /// kind + emitter win.
    pub fn register_metric<S: FrameSink>(
        &mut self,
        name: &'static str,
        kind: MetricKind,
        task_id: u32,
        sink: &mut S,
    ) -> StringId {
        let (id, is_new) = self.lookup_or_insert(name);
        if is_new {
            sink.emit(&Frame::StringRegister { id, value: name });
        }
        let entry = self
            .entry_mut(id.0 as usize)
            .expect("lookup_or_insert guarantees the slot is populated");
        if !entry.metric_registered {
            entry.metric_registered = true;
            sink.emit(&Frame::MetricRegister { name_id: id, kind, task_id });
        }
        id
    }

    /// Number of distinct names currently *live* — tombstoned (released) slots
    /// are excluded, so this drops when a process's names are reclaimed on exit.
    /// O(n) over the table (n is small, bounded; called ~once per heartbeat for
    /// the `snitchos.intern.strings_used` metric).
    pub fn count(&self) -> u32 {
        self.iter().count() as u32
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
    fn grows_past_the_inline_cap_into_the_heap_overflow() {
        let mut table = InternTable::new();
        let mut sink = CapturingSink::new();
        // Register well past INLINE_CAP (and past the old fixed 128 cap): the
        // inline region fills, then entries spill into the heap-backed Vec.
        // The old fixed-array table panicked here; the hybrid grows.
        let count = 200usize;
        let mut ids = Vec::new();
        for i in 0..count {
            let s: &'static str = Box::leak(format!("name{i}").into_boxed_str());
            ids.push(table.register_or_lookup(s, &mut sink));
        }
        assert_eq!(table.count(), count as u32);
        // Every name — inline and spilled — still resolves to its dense id.
        for (i, id) in ids.iter().enumerate() {
            assert_eq!(table.lookup_by_content(&format!("name{i}")), Some(*id));
        }
        // The first spilled entry sits immediately past the inline region.
        assert_eq!(ids[INLINE_CAP], StringId(INLINE_CAP as u32));
    }

    #[test]
    fn register_metric_resolves_a_spilled_overflow_entry() {
        // Exercises `entry_mut` across the region boundary: the metric name
        // lands in the heap overflow (id >= INLINE_CAP), and a second call
        // must find the *same* entry (metric_registered already set) rather
        // than a mis-indexed one — pinning the overflow index arithmetic.
        let mut table = InternTable::new();
        let mut sink = CapturingSink::new();
        for i in 0..INLINE_CAP {
            let s: &'static str = Box::leak(format!("filler{i}").into_boxed_str());
            table.register_or_lookup(s, &mut sink);
        }
        let m: &'static str = Box::leak(Box::<str>::from("overflow.metric"));
        let before = sink.len();

        let id1 = table.register_metric(m, MetricKind::Counter, 0, &mut sink);
        assert!(id1.0 as usize >= INLINE_CAP, "name must land in overflow");
        assert_eq!(sink.len(), before + 2, "first: StringRegister + MetricRegister");

        let id2 = table.register_metric(m, MetricKind::Counter, 0, &mut sink);
        assert_eq!(id1, id2);
        assert_eq!(sink.len(), before + 2, "second call must re-find the entry, not re-emit");
    }

    #[test]
    fn register_metric_stamps_the_registering_task_on_the_frame() {
        // The emitter dimension: the `MetricRegister` carries the task that
        // registered the metric, so the collector can keep two same-named metrics
        // from different processes as distinct series.
        let mut table = InternTable::new();
        let mut sink = CapturingSink::new();
        table.register_metric("m", MetricKind::Gauge, 42, &mut sink);
        // [0] = StringRegister (new name), [1] = MetricRegister (first declaration).
        assert!(matches!(
            decode(&sink.raw()[1]),
            Frame::MetricRegister { kind: MetricKind::Gauge, task_id: 42, .. }
        ));
    }

    #[test]
    fn register_counter_then_gauge_does_not_redeclare_kind() {
        // Programmer error mode, documented in the source: the second
        // call sees `metric_registered: true` and skips emit. Host
        // never learns the new kind. Test pins this so we don't
        // silently "fix" it without an explicit decision.
        let mut table = InternTable::new();
        let mut sink = CapturingSink::new();

        table.register_metric("m", MetricKind::Counter, 0, &mut sink);
        table.register_metric("m", MetricKind::Gauge, 0, &mut sink);

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
        table.register_metric("m", MetricKind::Counter, 0, &mut sink);

        assert_eq!(sink.len(), 2);
        assert!(matches!(decode(&sink.raw()[0]), Frame::StringRegister { .. }));
        assert!(matches!(decode(&sink.raw()[1]), Frame::MetricRegister { .. }));
    }

    #[test]
    fn register_owned_interns_a_heap_name_and_resolves_by_content() {
        // Userspace names are reclaimable: the table *owns* them (`Box<str>`)
        // rather than holding a `Box::leak`'d `&'static`, so they can later be
        // dropped on process exit. The owned name registers + resolves exactly
        // like a borrowed one.
        let mut table = InternTable::new();
        let mut sink = CapturingSink::new();

        let id = table.register_owned(Box::<str>::from("proc.span"), &mut sink);

        assert_eq!(table.lookup_by_content("proc.span"), Some(id));
        assert_eq!(
            decode(&sink.raw()[0]),
            Frame::StringRegister { id, value: "proc.span" },
        );
    }

    #[test]
    fn register_metric_owned_interns_a_reclaimable_metric_name() {
        // The userspace-metric path: owns the name (reclaimable on exit), always
        // appends (no cross-process dedup — the forgery boundary), and emits both
        // StringRegister and the emitter-stamped MetricRegister.
        let mut table = InternTable::new();
        let mut sink = CapturingSink::new();

        let id =
            table.register_metric_owned(Box::<str>::from("proc.metric"), MetricKind::Counter, 5, &mut sink);

        assert_eq!(sink.len(), 2);
        assert_eq!(
            decode(&sink.raw()[0]),
            Frame::StringRegister { id, value: "proc.metric" },
        );
        assert!(matches!(
            decode(&sink.raw()[1]),
            Frame::MetricRegister { name_id, kind: MetricKind::Counter, task_id: 5 } if name_id == id,
        ));

        table.release(id);
        assert_eq!(table.lookup_by_content("proc.metric"), None, "reclaimable on exit");
    }

    #[test]
    fn releasing_a_name_frees_it_and_never_reuses_the_id() {
        // Process exit reclaims its names. `release` drops the owned bytes and
        // tombstones the slot — but the id is a wire identity (the collector maps
        // id→name, frames cite ids), so it must NEVER be reused, or a new name
        // would silently alias the freed one.
        let mut table = InternTable::new();
        let mut sink = CapturingSink::new();

        let gone = table.register_owned(Box::<str>::from("ephemeral"), &mut sink);
        table.release(gone);

        assert_eq!(table.lookup_by_content("ephemeral"), None, "released name is gone");

        let fresh = table.register_owned(Box::<str>::from("fresh"), &mut sink);
        assert_ne!(fresh, gone, "a tombstoned id is never reused");
    }

    #[test]
    fn releasing_a_spilled_overflow_name_frees_it_and_drops_the_count() {
        // Exercises `release` across the region boundary (id >= INLINE_CAP): the
        // tombstone must land in the heap overflow, not a mis-indexed slot, and
        // `count` must reflect the freed name. The inline-id test above can't
        // reach this path.
        let mut table = InternTable::new();
        let mut sink = CapturingSink::new();
        for i in 0..INLINE_CAP {
            let s: &'static str = Box::leak(format!("filler{i}").into_boxed_str());
            table.register_or_lookup(s, &mut sink);
        }
        let spilled = table.register_owned(Box::<str>::from("spilled.name"), &mut sink);
        assert!(spilled.0 as usize >= INLINE_CAP, "name must land in overflow");
        let before = table.count();

        table.release(spilled);

        assert_eq!(table.lookup_by_content("spilled.name"), None);
        assert_eq!(table.count(), before - 1, "the freed overflow name is uncounted");
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
