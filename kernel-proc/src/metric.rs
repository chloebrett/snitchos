//! Per-process metric table — the userspace-defined-metrics security boundary.
//!
//! A process names its own metrics. It [`register`](MetricTable::register)s a
//! [`StringId`] (the kernel-interned metric name) and gets back an opaque
//! [`MetricHandle`] — an index into *this process's own table*. To emit, it
//! hands the kernel the handle, which [`resolve`](MetricTable::resolve)s it
//! back to the `StringId` to emit under. A handle is meaningless except as a
//! lookup into the table that issued it: a process can only emit to a metric
//! it registered, never forge another's (in particular, never the kernel's own
//! telemetry). This per-process table with no cross-process dedup is the
//! security fix from Post 31 — the "poisonable snitch" finding.
//!
//! The table is bounded by [`MetricTable::MAX_METRIC_NAMES`]: the capacity *is*
//! the quota (Q4), mirroring [`SpanNameTable`](super::span_name::SpanNameTable),
//! so a misbehaving program can't pin unbounded interned names *per process*. The
//! interned names are **reclaimed on process exit** — `reap_task` walks
//! [`ids`](MetricTable::ids) and releases each from the global intern table. (No
//! cross-process dedup, so each spawn registers its own; reclaim is what keeps a
//! long-running spawner bounded. See `plans/legacy/span-and-metric-name-gc.md`.)
//!
//! Pure data + bookkeeping: no `unsafe`, no MMIO, no CSRs. Host-tested here;
//! the `kernel` side only decides *where the table lives* (the process struct)
//! and wires the `RegisterMetric` / `EmitMetric` syscall arms.
//! See `plans/legacy/userspace-defined-metrics.md`.

use alloc::vec::Vec;

use protocol::StringId;

/// An opaque reference to a metric *within the table that issued it* — an
/// index into the issuing process's [`MetricTable`]. Carried across the
/// syscall boundary as a bare `u32` register, so [`from_raw`](Self::from_raw)
/// / [`raw`](Self::raw) round-trip it. Any `u32` is a syntactically valid
/// handle; [`MetricTable::resolve`] decides whether it names anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MetricHandle(u32);

impl MetricHandle {
    /// Rebuild a handle from the raw `u32` a syscall delivered in a register.
    #[must_use]
    pub const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    /// The raw `u32` to hand back across the syscall boundary.
    #[must_use]
    pub const fn raw(self) -> u32 {
        self.0
    }
}

/// A process's metric table: [`StringId`] names in, opaque [`MetricHandle`]s
/// out, validated against this table alone. Names are append-only (no
/// revocation), and the table is bounded by [`MAX_METRIC_NAMES`](Self::MAX_METRIC_NAMES)
/// — the capacity *is* the per-process quota.
#[derive(Debug, Default)]
pub struct MetricTable {
    names: Vec<StringId>,
}

impl MetricTable {
    /// Cap on distinct metrics a single process may register. Mirrors
    /// `Process::MAX_SPAN_NAMES`: generous for a real program, small enough
    /// that a misbehaving one can't pin unbounded interned names.
    pub const MAX_METRIC_NAMES: usize = 16;

    #[must_use]
    pub const fn new() -> Self {
        Self { names: Vec::new() }
    }

    /// Register `name` and return the handle that names it, or `None` if the
    /// table is already at [`MAX_METRIC_NAMES`](Self::MAX_METRIC_NAMES). The
    /// handle is the slot index — distinct per registration.
    pub fn register(&mut self, name: StringId) -> Option<MetricHandle> {
        if self.is_full() {
            return None;
        }
        let index = self.names.len() as u32;
        self.names.push(name);
        Some(MetricHandle(index))
    }

    /// Whether the table is at its [`MAX_METRIC_NAMES`](Self::MAX_METRIC_NAMES)
    /// quota — the next [`register`](Self::register) would return `None`. The
    /// register syscall checks this *before* leaking + interning the metric
    /// name, so a quota-refused registration commits no permanent `'static`
    /// string.
    #[must_use]
    pub fn is_full(&self) -> bool {
        self.names.len() >= Self::MAX_METRIC_NAMES
    }

    /// Resolve a handle to the [`StringId`] it names, validating it against
    /// this table. A handle this table never issued (out of range) resolves
    /// `None`, never another metric — this is the trust boundary that stops a
    /// process emitting to a metric it didn't register.
    #[must_use]
    pub fn resolve(&self, handle: MetricHandle) -> Option<StringId> {
        self.names.get(handle.0 as usize).copied()
    }

    /// Every [`StringId`] this process registered as a metric, for exit-time
    /// reclaim: the kernel releases each from the intern table when the process
    /// is reaped. Append-only and never tombstoned in this table — the handle
    /// indices that resolve to them must stay stable for the process's lifetime.
    #[must_use]
    pub fn ids(&self) -> &[StringId] {
        &self.names
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::StringId;

    #[test]
    fn a_registered_name_resolves_to_its_string_id() {
        let mut table = MetricTable::new();
        let h = table
            .register(StringId(7))
            .expect("the first registration is under the cap");
        assert_eq!(table.resolve(h), Some(StringId(7)));
    }

    #[test]
    fn ids_lists_every_registered_string_id_for_teardown() {
        // On process exit the kernel walks these ids and releases each from the
        // intern table, reclaiming the per-process metric names.
        let mut table = MetricTable::new();
        table.register(StringId(7)).expect("under the cap");
        table.register(StringId(9)).expect("under the cap");
        assert_eq!(table.ids(), &[StringId(7), StringId(9)]);
    }

    #[test]
    fn each_registration_yields_a_distinct_handle_resolving_to_its_own_name() {
        let mut table = MetricTable::new();
        let first = table.register(StringId(1)).expect("under the cap");
        let second = table.register(StringId(2)).expect("under the cap");

        assert_ne!(first, second);
        assert_eq!(table.resolve(first), Some(StringId(1)));
        assert_eq!(table.resolve(second), Some(StringId(2)));
    }

    #[test]
    fn registration_succeeds_up_to_the_cap_then_refuses_the_next() {
        let mut table = MetricTable::new();
        for i in 0..MetricTable::MAX_METRIC_NAMES {
            assert!(
                table.register(StringId(i as u32)).is_some(),
                "registration {i} is within the cap"
            );
        }
        assert_eq!(
            table.register(StringId(9999)),
            None,
            "one past the cap is refused"
        );
    }

    #[test]
    fn a_fresh_table_is_not_full_and_a_capped_table_is() {
        // The register syscall checks `is_full` *before* leaking + interning a
        // name, so a refused-for-quota registration commits no `'static` string.
        let mut table = MetricTable::new();
        assert!(!table.is_full(), "an empty table has room");
        for i in 0..MetricTable::MAX_METRIC_NAMES {
            assert!(!table.is_full(), "the table has room before registration {i}");
            table.register(StringId(i as u32)).expect("under the cap");
        }
        assert!(table.is_full(), "the table is full at the cap");
    }

    #[test]
    fn a_handle_this_table_never_issued_does_not_resolve() {
        let table = MetricTable::new();
        assert_eq!(table.resolve(MetricHandle::from_raw(0)), None);
    }

    #[test]
    fn a_handle_round_trips_through_its_raw_register_value() {
        // The syscall ABI carries a handle as a bare `u32`; the kernel rebuilds
        // it with `from_raw`. That must preserve the index, so the rebuilt
        // handle resolves to the same name. Slot 2 (not 0 or 1) so a stubbed
        // `raw()` returning a small constant resolves to the wrong name.
        let mut table = MetricTable::new();
        table.register(StringId(10)).expect("under the cap");
        table.register(StringId(20)).expect("under the cap");
        let third = table.register(StringId(30)).expect("under the cap");
        assert_ne!(third.raw(), 0);

        let rebuilt = MetricHandle::from_raw(third.raw());

        assert_eq!(rebuilt, third);
        assert_eq!(table.resolve(rebuilt), Some(StringId(30)));
    }

    #[test]
    fn a_handle_one_past_the_last_issued_does_not_resolve() {
        // Guards the bounds check (`<` vs `<=`): the index just beyond the
        // names registered so far names nothing, even though lower indices do.
        let mut table = MetricTable::new();
        let h = table.register(StringId(1)).expect("under the cap");
        let beyond = MetricHandle::from_raw(h.raw() + 1);
        assert_eq!(table.resolve(beyond), None);
    }
}
