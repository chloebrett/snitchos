//! Per-process capability table — the v0.7b authority primitive.
//!
//! A capability is an unforgeable `{ object, rights }` pair. A process
//! holds a [`CapTable`]; every kernel-mediated resource access names a
//! capability by an opaque [`Handle`] that the kernel **validates against
//! that process's own table**. A handle is meaningless except as a
//! lookup into the table that issued it — like a Unix fd, but the kernel
//! checks every use.
//!
//! Pure data + bookkeeping: no `unsafe`, no MMIO, no CSRs. Host-tested
//! here; the `kernel` side only decides *where the table lives* (the
//! process struct) and wires the syscall trap arm to [`CapTable::resolve`].
//! See `plans/v0.7b-capabilities.md`.

use alloc::vec::Vec;

use protocol::StringId;

/// The rights a capability grants, as a bitmask. v0.7b defines exactly
/// one bit (`EMIT`); the type is the extension point so later objects
/// (`Endpoint`, `File`) add bits without reshaping the capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rights(u32);

impl Rights {
    /// The empty set — a capability that grants nothing. The floor of
    /// attenuation; useful as a "held but powerless" cap in tests.
    pub const NONE: Rights = Rights(0);

    /// May emit telemetry through a [`Object::TelemetrySink`].
    // `1 << 0` reads as "bit 0" (the next right will be `1 << 1`).
    // cargo-mutants flags `<< → >>` here as a survivor; it is an
    // equivalent mutant — `1 << 0 == 1 >> 0 == 1` — not a test gap.
    pub const EMIT: Rights = Rights(1 << 0);

    /// Whether `self` grants every right in `other`.
    #[must_use]
    pub const fn contains(self, other: Rights) -> bool {
        self.0 & other.0 == other.0
    }
}

/// What a capability points at. One variant in v0.7b; the enum is the
/// extension point for the object zoo (`Endpoint`, `MemoryRegion`, …).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Object {
    /// Permission to emit telemetry, bound at creation to a specific
    /// kernel-registered counter. The userspace caller passes only a
    /// value; identity is kernel-stamped and the counter is named by the
    /// capability, so no string crosses the syscall boundary.
    TelemetrySink { counter: StringId },
}

/// An unforgeable `{ object, rights }` pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capability {
    pub object: Object,
    pub rights: Rights,
}

/// An opaque reference to a capability *within the table that issued it*.
/// In v0.7b a handle is a bare slot index; Step 2 packs a generation tag
/// alongside so a reused slot cannot alias a stale handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Handle(u32);

/// Why a handle failed to resolve. Every variant is a *rejection* the
/// kernel returns to U-mode — never a panic, never the wrong capability.
/// v0.7b has one cause; Step 2's generation tag adds `Stale`, and
/// revocation (deferred) would add an empty-slot cause.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapError {
    /// The index names no slot in this table.
    OutOfBounds,
}

/// A process's capability table: opaque [`Handle`]s in, [`Capability`]
/// references out, validated against this table alone. Slots are never
/// emptied in v0.7b (no revocation), so a present index always holds a
/// capability.
#[derive(Debug, Default)]
pub struct CapTable {
    slots: Vec<Capability>,
}

impl CapTable {
    #[must_use]
    pub const fn new() -> Self {
        Self { slots: Vec::new() }
    }

    /// Place `cap` in the table and return the handle that names it.
    /// Used at bootstrap to grant a process its root capabilities.
    pub fn insert(&mut self, cap: Capability) -> Handle {
        let index = self.slots.len();
        self.slots.push(cap);
        Handle(index as u32)
    }

    /// Resolve a handle to the capability it names, validating it against
    /// this table. A bad handle is a [`CapError`], not a panic — this is
    /// the trust boundary between U-mode and the kernel.
    pub fn resolve(&self, handle: Handle) -> Result<&Capability, CapError> {
        self.slots.get(handle.0 as usize).ok_or(CapError::OutOfBounds)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::StringId;

    #[test]
    fn a_resolved_handle_returns_the_capability_that_was_inserted() {
        let mut table = CapTable::new();
        let handle = table.insert(Capability {
            object: Object::TelemetrySink { counter: StringId(7) },
            rights: Rights::EMIT,
        });

        let cap = table.resolve(handle).expect("a freshly inserted handle resolves");

        assert!(cap.rights.contains(Rights::EMIT));
        let Object::TelemetrySink { counter } = cap.object;
        assert_eq!(counter, StringId(7));
    }

    fn emit_sink() -> Capability {
        Capability {
            object: Object::TelemetrySink { counter: StringId(1) },
            rights: Rights::EMIT,
        }
    }

    #[test]
    fn a_handle_past_the_end_of_the_table_is_out_of_bounds() {
        let table = CapTable::new();
        assert_eq!(table.resolve(Handle(0)), Err(CapError::OutOfBounds));
    }

    #[test]
    fn each_insert_yields_a_distinct_handle_resolving_to_its_own_capability() {
        let mut table = CapTable::new();
        let first = table.insert(emit_sink());
        let second = table.insert(Capability {
            object: Object::TelemetrySink { counter: StringId(2) },
            rights: Rights::EMIT,
        });

        assert_ne!(first, second);
        assert_eq!(table.resolve(first).unwrap().object, Object::TelemetrySink { counter: StringId(1) });
        assert_eq!(table.resolve(second).unwrap().object, Object::TelemetrySink { counter: StringId(2) });
    }

    #[test]
    fn a_capability_without_emit_does_not_pass_the_emit_check() {
        let cap = Capability {
            object: Object::TelemetrySink { counter: StringId(1) },
            rights: Rights::NONE,
        };
        assert!(!cap.rights.contains(Rights::EMIT));
    }

    #[test]
    fn the_same_object_can_be_held_with_different_rights() {
        // Attenuation's mechanism (unused by the boolean TelemetrySink,
        // but the table must not collapse two caps to one object).
        let mut table = CapTable::new();
        let full = table.insert(emit_sink());
        let attenuated = table.insert(Capability {
            object: Object::TelemetrySink { counter: StringId(1) },
            rights: Rights::NONE,
        });

        assert!(table.resolve(full).unwrap().rights.contains(Rights::EMIT));
        assert!(!table.resolve(attenuated).unwrap().rights.contains(Rights::EMIT));
    }
}
