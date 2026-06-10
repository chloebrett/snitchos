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

    /// May emit telemetry through a [`Object::TelemetrySink`]. Bits are
    /// written as binary literals (next rights: `0b0010`, `0b0100`, …) —
    /// no shift to misread, and no no-op `1 << 0` for mutation testing to
    /// flag as an equivalent mutant.
    pub const EMIT: Rights = Rights(0b0001);

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
///
/// Packs a slot `index` (low [`Handle::INDEX_BITS`] bits) and a
/// `generation` (the rest) into one `u32` — the width the syscall ABI
/// carries in a register. The generation is dead weight in v0.7b
/// (nothing frees a slot, so it is always 0), but it is what lets a
/// future revocation reuse a slot without a stale handle aliasing the new
/// occupant: bump the slot's generation and every old handle to it fails
/// [`CapTable::resolve`] with [`CapError::Stale`]. Cheap now, expensive to
/// retrofit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Handle(u32);

impl Handle {
    /// Bits of the packed `u32` given to the slot index. 16 indexes far
    /// more capabilities than any v0.7b process holds and leaves 16 bits
    /// of generation — generous on both axes for a tiny table.
    const INDEX_BITS: u32 = 16;
    const INDEX_MASK: u32 = (1 << Self::INDEX_BITS) - 1;

    fn new(index: u32, generation: u32) -> Self {
        Self((generation << Self::INDEX_BITS) | (index & Self::INDEX_MASK))
    }

    fn index(self) -> u32 {
        self.0 & Self::INDEX_MASK
    }

    fn generation(self) -> u32 {
        self.0 >> Self::INDEX_BITS
    }

    /// Rebuild a handle from the raw `u32` the syscall ABI delivered in a
    /// register. Total: any `u32` is a syntactically valid handle;
    /// [`CapTable::resolve`] is what decides whether it names anything.
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

/// Why a handle failed to resolve. Every variant is a *rejection* the
/// kernel returns to U-mode — never a panic, never the wrong capability.
/// v0.7b has one cause; Step 2's generation tag adds `Stale`, and
/// revocation (deferred) would add an empty-slot cause.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapError {
    /// The index names no slot in this table.
    OutOfBounds,
    /// The slot exists, but the handle's generation does not match it —
    /// a handle outliving the slot it named. (Cannot occur in v0.7b,
    /// where nothing bumps a generation; the check guards future
    /// revocation.)
    Stale,
}

/// One occupied slot: the capability plus the generation a valid handle
/// to it must carry.
#[derive(Debug, Clone, Copy)]
struct Slot {
    generation: u32,
    cap: Capability,
}

/// Why a capability *invocation* was refused. Distinct from [`CapError`]
/// (which only covers handle resolution): an invocation can also fail
/// because the named capability lacks the right the operation needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Denied {
    /// The handle named no live capability (out of bounds or stale).
    NoSuchCapability,
    /// The capability exists but lacks the right this operation requires.
    MissingRight,
}

/// Resolve a `TelemetrySink` invocation: `handle` must name a capability
/// in `table` carrying [`Rights::EMIT`] over an [`Object::TelemetrySink`].
/// Returns the bound counter the kernel should emit to, or why the
/// invocation is refused.
///
/// This is the v0.7b authority decision, pure and host-tested. The kernel
/// side only acts on the result: emit on `Ok`, snitch + return an error
/// code on `Err`. (One object type today, so the object match is
/// irrefutable; it becomes a real discriminator when the object zoo grows.)
pub fn invoke_telemetry(table: &CapTable, handle: Handle) -> Result<StringId, Denied> {
    let cap = table.resolve(handle).map_err(|_| Denied::NoSuchCapability)?;
    if !cap.rights.contains(Rights::EMIT) {
        return Err(Denied::MissingRight);
    }
    let Object::TelemetrySink { counter } = cap.object;
    Ok(counter)
}

/// A process's capability table: opaque [`Handle`]s in, [`Capability`]
/// references out, validated against this table alone. Slots are never
/// emptied in v0.7b (no revocation), so a present index always holds a
/// capability — but each carries a generation so a stale handle is
/// rejected rather than aliased once revocation lands.
#[derive(Debug, Default)]
pub struct CapTable {
    slots: Vec<Slot>,
}

impl CapTable {
    #[must_use]
    pub const fn new() -> Self {
        Self { slots: Vec::new() }
    }

    /// Build the initial capability table for the v0.7b `init` process:
    /// exactly one [`Object::TelemetrySink`] bound to `counter`, with
    /// `EMIT`. Returns the table and the handle the sink landed at (the
    /// well-known bootstrap handle the user program invokes). This is the
    /// "root cap to init only" policy — the *only* authority a userspace
    /// process is born with in v0.7b.
    #[must_use]
    pub fn bootstrap_telemetry(counter: StringId) -> (Self, Handle) {
        let mut table = Self::new();
        let handle = table.insert(Capability {
            object: Object::TelemetrySink { counter },
            rights: Rights::EMIT,
        });
        (table, handle)
    }

    /// Place `cap` in the table and return the handle that names it.
    /// Used at bootstrap to grant a process its root capabilities.
    pub fn insert(&mut self, cap: Capability) -> Handle {
        let index = self.slots.len() as u32;
        let generation = 0; // fresh slot; revocation would advance this
        self.slots.push(Slot { generation, cap });
        Handle::new(index, generation)
    }

    /// Resolve a handle to the capability it names, validating it against
    /// this table. A bad handle is a [`CapError`], not a panic — this is
    /// the trust boundary between U-mode and the kernel. Bounds are
    /// checked first, then the generation: a present slot with a
    /// mismatched generation is [`CapError::Stale`], never the wrong
    /// capability.
    pub fn resolve(&self, handle: Handle) -> Result<&Capability, CapError> {
        let slot = self.slots.get(handle.index() as usize).ok_or(CapError::OutOfBounds)?;
        if slot.generation != handle.generation() {
            return Err(CapError::Stale);
        }
        Ok(&slot.cap)
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
    fn invoking_a_granted_telemetry_sink_yields_its_bound_counter() {
        let counter = StringId(99);
        let (table, handle) = CapTable::bootstrap_telemetry(counter);

        assert_eq!(invoke_telemetry(&table, handle), Ok(counter));
    }

    #[test]
    fn invoking_an_unknown_handle_is_denied_as_no_such_capability() {
        let table = CapTable::new();
        assert_eq!(invoke_telemetry(&table, Handle::from_raw(0)), Err(Denied::NoSuchCapability));
    }

    #[test]
    fn invoking_a_stale_handle_is_denied_as_no_such_capability() {
        let (table, handle) = CapTable::bootstrap_telemetry(StringId(1));
        let stale = Handle::new(handle.index(), handle.generation() + 1);
        assert_eq!(invoke_telemetry(&table, stale), Err(Denied::NoSuchCapability));
    }

    #[test]
    fn invoking_a_capability_that_lacks_emit_is_denied_for_the_missing_right() {
        let mut table = CapTable::new();
        let handle = table.insert(Capability {
            object: Object::TelemetrySink { counter: StringId(1) },
            rights: Rights::NONE,
        });
        assert_eq!(invoke_telemetry(&table, handle), Err(Denied::MissingRight));
    }

    #[test]
    fn the_bootstrap_grant_gives_init_exactly_one_emit_telemetry_sink() {
        // The "root cap to init only" policy, pinned: granting the wrong
        // rights or object here is a privilege bug, so it's host-tested
        // rather than left to the itest.
        let counter = StringId(42);
        let (table, handle) = CapTable::bootstrap_telemetry(counter);

        let cap = table.resolve(handle).expect("the bootstrap handle resolves");
        assert_eq!(cap.object, Object::TelemetrySink { counter });
        assert!(cap.rights.contains(Rights::EMIT));
        // The single bootstrap grant lands at the well-known handle the
        // user program is told to invoke (Step 4's `TELEMETRY_SINK_HANDLE`).
        assert_eq!(handle.raw(), 0);
    }

    #[test]
    fn a_handle_with_a_stale_generation_is_rejected_not_aliased_to_the_slot() {
        // The slot exists and holds a capability, but the handle's
        // generation does not match the slot's. This is the case a future
        // revocation creates (free slot, bump generation, reuse): a stale
        // handle must be refused, never silently resolve to whatever now
        // lives in that slot.
        let mut table = CapTable::new();
        let valid = table.insert(emit_sink());
        let stale = Handle::new(valid.index(), valid.generation() + 1);

        assert_eq!(table.resolve(stale), Err(CapError::Stale));
        assert!(table.resolve(valid).is_ok());
    }

    #[test]
    fn a_handle_survives_a_round_trip_through_its_raw_register_value() {
        // The syscall ABI carries a handle as a bare `u32` in a register;
        // the kernel rebuilds it with `from_raw`. That must preserve both
        // index and generation, so the rebuilt handle resolves identically.
        let mut table = CapTable::new();
        table.insert(emit_sink()); // occupy slot 0 so the handle under test isn't `raw == 0`
        let handle = table.insert(Capability {
            object: Object::TelemetrySink { counter: StringId(2) },
            rights: Rights::EMIT,
        });
        // Guard: a zero raw value would let a stubbed `raw()` pass vacuously.
        assert_ne!(handle.raw(), 0);

        let rebuilt = Handle::from_raw(handle.raw());

        assert_eq!(rebuilt, handle);
        assert_eq!(table.resolve(rebuilt), table.resolve(handle));
    }

    #[test]
    fn a_handle_packs_and_unpacks_both_index_and_generation() {
        // Non-trivial values in *both* fields: a stubbed accessor (→ 0/1),
        // a flipped shift, or a wrong mask all change the result, so the
        // packing is pinned rather than coincidentally matching a small id.
        let handle = Handle::new(5, 3);

        assert_eq!(handle.index(), 5);
        assert_eq!(handle.generation(), 3);
        assert_eq!(Handle::from_raw(handle.raw()), handle);
        assert_ne!(handle.raw(), 0);
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
