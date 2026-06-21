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

use crate::ipc::EndpointId;
use crate::sched::TaskId;

/// The rights a capability grants, as a bitmask. v0.7b defines exactly
/// one bit (`EMIT`); the type is the extension point so later objects
/// (`Endpoint`, `File`) add bits without reshaping the capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rights(u32);

impl Rights {
    /// The empty set — a capability that grants nothing. The floor of
    /// attenuation; useful as a "held but powerless" cap in tests.
    /// Also used for Object::Reply.
    pub const NONE: Rights = Rights(0);

    /// May emit telemetry through a [`Object::TelemetrySink`]. The bit values
    /// live in [`snitchos_abi::rights`] — the single source of truth shared with
    /// userspace, so neither side hard-codes them.
    pub const EMIT: Rights = Rights(snitchos_abi::rights::EMIT);

    /// May `send` on an [`Object::Endpoint`] (v0.9 IPC).
    pub const SEND: Rights = Rights(snitchos_abi::rights::SEND);

    /// May `receive` on an [`Object::Endpoint`] (v0.9 IPC). Disjoint from
    /// `SEND` so a cap can grant one, the other, or both (`SEND | RECV`).
    pub const RECV: Rights = Rights(snitchos_abi::rights::RECV);

    /// May derive badged `SEND` caps naming an [`Object::Endpoint`] the holder
    /// owns (v0.9c). The endpoint owner (a server) holds `RECV | MINT` and
    /// stamps each derived client cap's badge + rights; clients hold no `MINT`,
    /// so they cannot mint (re-delegation is a deferred follow-on).
    pub const MINT: Rights = Rights(snitchos_abi::rights::MINT);

    /// Whether `self` grants every right in `other`.
    #[must_use]
    pub const fn contains(self, other: Rights) -> bool {
        self.0 & other.0 == other.0
    }

    /// The raw bitmask, for placing on the `CapEvent` wire frame.
    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// Rebuild a rights set from the raw bits a syscall delivered in a register
    /// (the `MintBadged` requested rights). The inverse of [`bits`](Self::bits).
    /// A `MINT`-holder sets a minted cap's rights freely, so this wraps the
    /// value verbatim — the kernel does not curate the bits.
    #[must_use]
    pub const fn from_bits(bits: u32) -> Rights {
        Rights(bits)
    }
}

impl core::ops::BitOr for Rights {
    type Output = Rights;
    /// Union two rights sets — e.g. a server endpoint owner holds
    /// `RECV | MINT` on one capability.
    fn bitor(self, rhs: Rights) -> Rights {
        Rights(self.0 | rhs.0)
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
    /// Permission to open and close spans on the holder's per-task span
    /// cursor. Carries no payload: the span *name* arrives per-call from
    /// userspace (interned kernel-side on demand), while the parent span and
    /// task id are kernel-stamped. Distinct from `TelemetrySink` so emitting
    /// a span and bumping a counter are separately granted authorities.
    SpanSink,
    /// A synchronous IPC endpoint (v0.9). The cap names *which* endpoint by
    /// [`EndpointId`]; `Rights::SEND` / `Rights::RECV` decide which end the
    /// holder may use. The rendezvous itself lives in `crate::ipc`.
    ///
    /// `badge` (v0.9c) is a server-chosen, kernel-opaque value the kernel
    /// delivers to the receiver on every message, so one endpoint can demux
    /// many objects/clients by capability rather than by sender identity.
    /// `badge == 0` is the *bare* endpoint (the owner/`RECV` cap, as before);
    /// a nonzero badge marks a derived `SEND` cap. The kernel never reads it
    /// beyond carrying it — all meaning is the server's.
    Endpoint { id: EndpointId, badge: u64 },
    /// A one-shot authority to **reply** to a blocked `call`er (v0.9b). Names
    /// the specific caller to wake. Minted by the kernel into the server at the
    /// `call` rendezvous and granted [`Multiplicity::Once`] — holding it *is*
    /// the authority (no rights bit), and answering consumes it.
    Reply { caller: TaskId },
}

/// An unforgeable `{ object, rights }` pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capability {
    pub object: Object,
    pub rights: Rights,
}

/// How many times a grant may be invoked — a property of the *grant* (the
/// table slot), orthogonal to its [`Rights`]. `Persistent` caps are invoked
/// repeatedly (telemetry, spans, endpoints); a `Once` cap is **consumed** on
/// its first successful invoke (the affine/linear-capability shape — v0.9b's
/// reply cap is the first instance). Two variants by design: no multiplicity
/// machinery until a second `Once` consumer (cap-transfer-in-messages) earns it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Multiplicity {
    /// Invoke any number of times.
    #[default]
    Persistent,
    /// Invoke exactly once, then the grant is consumed (its handle goes stale).
    Once,
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

/// One table slot: the generation a valid handle must carry, the grant's
/// [`Multiplicity`], and the capability — `None` once the slot has been
/// consumed/freed (a tombstone awaiting reuse, so live indices stay stable).
#[derive(Debug, Clone, Copy)]
struct Slot {
    generation: u32,
    multiplicity: Multiplicity,
    cap: Option<Capability>,
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
    /// The capability resolves and has the right, but names a different
    /// object kind than the operation targets — e.g. a span op handed a
    /// `TelemetrySink` handle, or vice versa. Holding the integer is not
    /// enough; it must name the *right kind* of thing.
    WrongObject,
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
    let cap = table
        .resolve(handle)
        .map_err(|_| Denied::NoSuchCapability)?;
    if !cap.rights.contains(Rights::EMIT) {
        return Err(Denied::MissingRight);
    }
    let Object::TelemetrySink { counter } = cap.object else {
        return Err(Denied::WrongObject);
    };
    Ok(counter)
}

/// Resolve a `SpanSink` invocation: `handle` must name a capability in
/// `table` carrying [`Rights::EMIT`] over an [`Object::SpanSink`]. Returns
/// `Ok(())` when the holder is authorized to open a span — name, parent, and
/// task id are all supplied kernel-side, so there is nothing to hand back.
/// Pure and host-tested, the span twin of [`invoke_telemetry`].
pub fn invoke_span(table: &CapTable, handle: Handle) -> Result<(), Denied> {
    let cap = table
        .resolve(handle)
        .map_err(|_| Denied::NoSuchCapability)?;
    if !cap.rights.contains(Rights::EMIT) {
        return Err(Denied::MissingRight);
    }
    let Object::SpanSink = cap.object else {
        return Err(Denied::WrongObject);
    };
    Ok(())
}

/// Resolve a `send` invocation: `handle` must name an [`Object::Endpoint`]
/// in `table` carrying [`Rights::SEND`]. Returns the target [`EndpointId`]
/// **and the cap's `badge`** — the kernel delivers the badge to the receiver
/// so it can demux the sender's object (v0.9c). The `recv` twin is
/// [`invoke_recv`]; both mirror [`invoke_telemetry`].
pub fn invoke_send(table: &CapTable, handle: Handle) -> Result<(EndpointId, u64), Denied> {
    let cap = table
        .resolve(handle)
        .map_err(|_| Denied::NoSuchCapability)?;
    if !cap.rights.contains(Rights::SEND) {
        return Err(Denied::MissingRight);
    }
    let Object::Endpoint { id, badge } = cap.object else {
        return Err(Denied::WrongObject);
    };
    Ok((id, badge))
}

/// Resolve a `receive` invocation: `handle` must name an [`Object::Endpoint`]
/// in `table` carrying [`Rights::RECV`]. The mirror of [`invoke_send`].
pub fn invoke_recv(table: &CapTable, handle: Handle) -> Result<EndpointId, Denied> {
    let cap = table
        .resolve(handle)
        .map_err(|_| Denied::NoSuchCapability)?;
    if !cap.rights.contains(Rights::RECV) {
        return Err(Denied::MissingRight);
    }
    let Object::Endpoint { id, .. } = cap.object else {
        return Err(Denied::WrongObject);
    };
    Ok(id)
}

/// Resolve a `reply` invocation: `handle` must name an [`Object::Reply`] in
/// `table`. Returns the [`TaskId`] of the blocked `call`er to wake. No rights
/// check — possession of the reply cap is the authority. The cap is granted
/// [`Multiplicity::Once`], so the kernel [`consume`](CapTable::consume)s it
/// after a successful invoke; a second `reply` then resolves `Stale` →
/// [`Denied::NoSuchCapability`].
pub fn invoke_reply(table: &CapTable, handle: Handle) -> Result<TaskId, Denied> {
    let cap = table
        .resolve(handle)
        .map_err(|_| Denied::NoSuchCapability)?;
    let Object::Reply { caller } = cap.object else {
        return Err(Denied::WrongObject);
    };
    Ok(caller)
}

/// Derive a badged `SEND` capability for the endpoint `parent` names (v0.9c).
/// `parent` must carry [`Rights::MINT`] over an [`Object::Endpoint`]; the
/// returned child names the *same* endpoint, stamped with `badge` and the
/// requested `rights`. The `MINT`-holder owns the endpoint, so it sets the
/// child's rights freely (not a subset of its own) — the kernel only checks
/// that it *may* mint. The child's [`Multiplicity`] is an insertion concern,
/// decided when the caller places it in a table; this pure derive returns only
/// the [`Capability`]. Mirrors the `invoke_*` resolvers.
pub fn mint_badged(parent: Capability, badge: u64, rights: Rights) -> Result<Capability, Denied> {
    if !parent.rights.contains(Rights::MINT) {
        return Err(Denied::MissingRight);
    }
    let Object::Endpoint { id, .. } = parent.object else {
        return Err(Denied::WrongObject);
    };
    Ok(Capability {
        object: Object::Endpoint { id, badge },
        rights,
    })
}

/// Resolve `handles` against `table` into the capabilities to delegate to a
/// freshly-spawned child — the v0.11 `Spawn`-with-caps core.
///
/// **All-or-nothing**: if any handle fails to resolve (out of bounds / stale),
/// no caps are returned and that error propagates. There is no partial
/// delegation, and a process can only delegate caps it *holds* — the kernel
/// forges nothing. **Copy** semantics: resolving doesn't disturb `table`, so the
/// parent keeps its caps (attenuation is mint-then-delegate, not a move). The
/// kernel inserts the returned caps into the child's new table alongside its
/// bootstrap authorities.
pub fn delegate(table: &CapTable, handles: &[Handle]) -> Result<Vec<Capability>, CapError> {
    handles
        .iter()
        .map(|handle| table.resolve(*handle).copied())
        .collect()
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

    /// Build the initial capability table for the `init` process: the two
    /// bootstrap authorities, each with `EMIT` over its own object — a
    /// [`Object::TelemetrySink`] bound to `counter`, then an
    /// [`Object::SpanSink`]. Returns the table and the two handles, in grant
    /// order (telemetry in slot 0, span in slot 1). This is the "root caps to
    /// init only" policy — the *only* authority a userspace process is born
    /// with.
    #[must_use]
    pub fn bootstrap(counter: StringId) -> (Self, Handle, Handle) {
        let mut table = Self::new();
        let telemetry = table.insert(Capability {
            object: Object::TelemetrySink { counter },
            rights: Rights::EMIT,
        });
        let span = table.insert(Capability {
            object: Object::SpanSink,
            rights: Rights::EMIT,
        });
        (table, telemetry, span)
    }

    /// Grant `cap` as a [`Multiplicity::Persistent`] capability and return the
    /// handle that names it. Used at bootstrap to grant a process its root
    /// capabilities.
    pub fn insert(&mut self, cap: Capability) -> Handle {
        self.grant(cap, Multiplicity::Persistent)
    }

    /// Grant `cap` as a single-use [`Multiplicity::Once`] capability: the first
    /// successful invoke should [`consume`](Self::consume) it. v0.9b's reply
    /// cap is the first user.
    pub fn insert_once(&mut self, cap: Capability) -> Handle {
        self.grant(cap, Multiplicity::Once)
    }

    /// Place `cap` in the table with `multiplicity`, reusing a freed slot if
    /// one exists (so a server consuming reply caps doesn't grow the table
    /// unboundedly) — otherwise appending. A reused slot keeps its bumped
    /// generation, so handles to its former occupant stay stale.
    fn grant(&mut self, cap: Capability, multiplicity: Multiplicity) -> Handle {
        if let Some((index, slot)) = self
            .slots
            .iter_mut()
            .enumerate()
            .find(|(_, s)| s.cap.is_none())
        {
            slot.multiplicity = multiplicity;
            slot.cap = Some(cap);
            return Handle::new(index as u32, slot.generation);
        }
        let index = self.slots.len() as u32;
        let generation = 0; // fresh slot
        self.slots.push(Slot {
            generation,
            multiplicity,
            cap: Some(cap),
        });
        Handle::new(index, generation)
    }

    /// Consume the capability `handle` names: free its slot and bump the
    /// generation so the handle (and any copy) now resolves [`CapError::Stale`],
    /// and the slot can be reused at the new generation. Returns whether a live
    /// capability was consumed. This is the consume step of a single-use
    /// capability — and the long-reserved revocation path.
    pub fn consume(&mut self, handle: Handle) -> bool {
        let Some(slot) = self.slots.get_mut(handle.index() as usize) else {
            return false;
        };
        if slot.generation != handle.generation() || slot.cap.is_none() {
            return false;
        }
        slot.cap = None;
        slot.generation = slot.generation.wrapping_add(1);
        true
    }

    /// The [`Multiplicity`] of the grant `handle` names, or why it doesn't
    /// resolve. The invoke path reads this to decide whether a successful
    /// invoke must [`consume`](Self::consume) the cap.
    pub fn multiplicity_of(&self, handle: Handle) -> Result<Multiplicity, CapError> {
        let slot = self
            .slots
            .get(handle.index() as usize)
            .ok_or(CapError::OutOfBounds)?;
        if slot.generation != handle.generation() {
            return Err(CapError::Stale);
        }
        if slot.cap.is_none() {
            return Err(CapError::Stale);
        }
        Ok(slot.multiplicity)
    }

    /// Resolve a handle to the capability it names, validating it against
    /// this table. A bad handle is a [`CapError`], not a panic — this is
    /// the trust boundary between U-mode and the kernel. Bounds are
    /// checked first, then the generation: a present slot with a
    /// mismatched generation is [`CapError::Stale`], never the wrong
    /// capability.
    pub fn resolve(&self, handle: Handle) -> Result<&Capability, CapError> {
        let slot = self
            .slots
            .get(handle.index() as usize)
            .ok_or(CapError::OutOfBounds)?;
        if slot.generation != handle.generation() {
            return Err(CapError::Stale);
        }
        // A freed (consumed) slot holds no capability. The generation bump on
        // consume means no live handle reaches here, but guard rather than
        // hand back a stale reference.
        slot.cap.as_ref().ok_or(CapError::Stale)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invoke_span_accepts_spansink_with_emit() {
        let mut table = CapTable::new();
        let h = table.insert(Capability {
            object: Object::SpanSink,
            rights: Rights::EMIT,
        });
        assert_eq!(invoke_span(&table, h), Ok(()));
    }

    #[test]
    fn invoke_span_refuses_spansink_without_emit() {
        let mut table = CapTable::new();
        let h = table.insert(Capability {
            object: Object::SpanSink,
            rights: Rights::NONE,
        });
        assert_eq!(invoke_span(&table, h), Err(Denied::MissingRight));
    }

    #[test]
    fn invoke_span_refuses_telemetry_sink_as_wrong_object() {
        let mut table = CapTable::new();
        let h = table.insert(Capability {
            object: Object::TelemetrySink {
                counter: StringId(1),
            },
            rights: Rights::EMIT,
        });
        assert_eq!(invoke_span(&table, h), Err(Denied::WrongObject));
    }

    #[test]
    fn invoke_span_refuses_unknown_handle() {
        let table = CapTable::new();
        assert_eq!(
            invoke_span(&table, Handle::from_raw(0)),
            Err(Denied::NoSuchCapability)
        );
    }

    #[test]
    fn invoke_send_accepts_an_endpoint_with_the_send_right() {
        let mut table = CapTable::new();
        let h = table.insert(Capability {
            object: Object::Endpoint {
                id: EndpointId(5),
                badge: 0,
            },
            rights: Rights::SEND,
        });
        assert_eq!(invoke_send(&table, h), Ok((EndpointId(5), 0)));
    }

    #[test]
    fn resolve_carries_the_badge_on_an_endpoint_cap() {
        let mut table = CapTable::new();
        let h = table.insert(Capability {
            object: Object::Endpoint {
                id: EndpointId(5),
                badge: 0xBEEF,
            },
            rights: Rights::SEND,
        });
        let cap = table.resolve(h).expect("freshly inserted cap resolves");
        assert_eq!(
            cap.object,
            Object::Endpoint {
                id: EndpointId(5),
                badge: 0xBEEF
            }
        );
    }

    #[test]
    fn invoke_send_returns_the_endpoint_and_the_senders_badge() {
        let mut table = CapTable::new();
        let h = table.insert(Capability {
            object: Object::Endpoint {
                id: EndpointId(5),
                badge: 0xBEEF,
            },
            rights: Rights::SEND,
        });
        assert_eq!(invoke_send(&table, h), Ok((EndpointId(5), 0xBEEF)));
    }

    #[test]
    fn invoke_send_refuses_an_endpoint_lacking_the_send_right() {
        let mut table = CapTable::new();
        let h = table.insert(Capability {
            object: Object::Endpoint {
                id: EndpointId(5),
                badge: 0,
            },
            rights: Rights::RECV,
        });
        assert_eq!(invoke_send(&table, h), Err(Denied::MissingRight));
    }

    #[test]
    fn invoke_send_refuses_a_telemetry_sink_as_wrong_object() {
        let mut table = CapTable::new();
        let h = table.insert(Capability {
            object: Object::TelemetrySink {
                counter: StringId(1),
            },
            rights: Rights::SEND,
        });
        assert_eq!(invoke_send(&table, h), Err(Denied::WrongObject));
    }

    #[test]
    fn invoke_send_refuses_an_unknown_handle() {
        let table = CapTable::new();
        assert_eq!(
            invoke_send(&table, Handle::from_raw(0)),
            Err(Denied::NoSuchCapability)
        );
    }

    #[test]
    fn invoke_recv_accepts_an_endpoint_with_the_recv_right() {
        let mut table = CapTable::new();
        let h = table.insert(Capability {
            object: Object::Endpoint {
                id: EndpointId(8),
                badge: 0,
            },
            rights: Rights::RECV,
        });
        assert_eq!(invoke_recv(&table, h), Ok(EndpointId(8)));
    }

    #[test]
    fn invoke_recv_refuses_an_endpoint_lacking_the_recv_right() {
        let mut table = CapTable::new();
        let h = table.insert(Capability {
            object: Object::Endpoint {
                id: EndpointId(8),
                badge: 0,
            },
            rights: Rights::SEND,
        });
        assert_eq!(invoke_recv(&table, h), Err(Denied::MissingRight));
    }

    #[test]
    fn the_send_and_recv_rights_are_distinct_bits() {
        assert_ne!(Rights::SEND.bits(), Rights::RECV.bits());
        assert_ne!(Rights::SEND.bits(), Rights::EMIT.bits());
        assert!(!Rights::SEND.contains(Rights::RECV));
    }

    #[test]
    fn the_mint_right_is_a_distinct_bit() {
        assert_ne!(Rights::MINT.bits(), Rights::EMIT.bits());
        assert_ne!(Rights::MINT.bits(), Rights::SEND.bits());
        assert_ne!(Rights::MINT.bits(), Rights::RECV.bits());
    }

    #[test]
    fn rights_combine_with_bitor() {
        // A server endpoint owner holds RECV | MINT on one cap.
        let combined = Rights::RECV | Rights::MINT;
        assert!(combined.contains(Rights::RECV));
        assert!(combined.contains(Rights::MINT));
        assert!(!combined.contains(Rights::SEND));
        // Idempotent: union of a right with itself is itself. Distinguishes OR
        // from XOR (which would collapse `R | R` to the empty set).
        assert_eq!(Rights::SEND | Rights::SEND, Rights::SEND);
    }

    #[test]
    fn rights_from_bits_round_trips_through_bits() {
        // The MintBadged syscall carries the requested rights as a raw u32
        // register; `from_bits` rebuilds the set the minter asked for.
        let r = Rights::from_bits(Rights::SEND.bits());
        assert_eq!(r, Rights::SEND);
        assert_eq!(
            Rights::from_bits((Rights::RECV | Rights::MINT).bits()),
            Rights::RECV | Rights::MINT
        );
    }

    #[test]
    fn mint_badged_derives_a_badged_send_cap_from_a_mint_parent() {
        // A MINT-holder owns the endpoint and sets the child's rights freely —
        // here a SEND child, though the parent itself holds only MINT.
        let parent = Capability {
            object: Object::Endpoint {
                id: EndpointId(7),
                badge: 0,
            },
            rights: Rights::MINT,
        };
        assert_eq!(
            mint_badged(parent, 0xF00D, Rights::SEND),
            Ok(Capability {
                object: Object::Endpoint {
                    id: EndpointId(7),
                    badge: 0xF00D
                },
                rights: Rights::SEND,
            }),
        );
    }

    #[test]
    fn mint_badged_refuses_a_parent_without_mint() {
        let parent = Capability {
            object: Object::Endpoint {
                id: EndpointId(7),
                badge: 0,
            },
            rights: Rights::RECV,
        };
        assert_eq!(
            mint_badged(parent, 0xF00D, Rights::SEND),
            Err(Denied::MissingRight)
        );
    }

    #[test]
    fn mint_badged_refuses_a_non_endpoint_parent() {
        let parent = Capability {
            object: Object::TelemetrySink {
                counter: StringId(1),
            },
            rights: Rights::MINT,
        };
        assert_eq!(
            mint_badged(parent, 0xF00D, Rights::SEND),
            Err(Denied::WrongObject)
        );
    }

    #[test]
    fn invoke_telemetry_refuses_spansink_as_wrong_object() {
        let mut table = CapTable::new();
        let h = table.insert(Capability {
            object: Object::SpanSink,
            rights: Rights::EMIT,
        });
        assert_eq!(invoke_telemetry(&table, h), Err(Denied::WrongObject));
    }
    use protocol::StringId;

    #[test]
    fn a_resolved_handle_returns_the_capability_that_was_inserted() {
        let mut table = CapTable::new();
        let handle = table.insert(Capability {
            object: Object::TelemetrySink {
                counter: StringId(7),
            },
            rights: Rights::EMIT,
        });

        let cap = table
            .resolve(handle)
            .expect("a freshly inserted handle resolves");

        assert!(cap.rights.contains(Rights::EMIT));
        let Object::TelemetrySink { counter } = cap.object else {
            panic!("bootstrap granted a non-TelemetrySink object");
        };
        assert_eq!(counter, StringId(7));
    }

    fn emit_sink() -> Capability {
        Capability {
            object: Object::TelemetrySink {
                counter: StringId(1),
            },
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
            object: Object::TelemetrySink {
                counter: StringId(2),
            },
            rights: Rights::EMIT,
        });

        assert_ne!(first, second);
        assert_eq!(
            table.resolve(first).unwrap().object,
            Object::TelemetrySink {
                counter: StringId(1)
            }
        );
        assert_eq!(
            table.resolve(second).unwrap().object,
            Object::TelemetrySink {
                counter: StringId(2)
            }
        );
    }

    #[test]
    fn rights_expose_their_raw_bits_for_the_wire() {
        // The CapEvent frame carries rights as a u32; the kernel reads them
        // off the granted capability rather than re-deriving the constant.
        assert_eq!(Rights::EMIT.bits(), 0b0001);
        assert_eq!(Rights::NONE.bits(), 0);
    }

    #[test]
    fn a_capability_without_emit_does_not_pass_the_emit_check() {
        let cap = Capability {
            object: Object::TelemetrySink {
                counter: StringId(1),
            },
            rights: Rights::NONE,
        };
        assert!(!cap.rights.contains(Rights::EMIT));
    }

    #[test]
    fn invoking_a_granted_telemetry_sink_yields_its_bound_counter() {
        let counter = StringId(99);
        let (table, handle, _span) = CapTable::bootstrap(counter);

        assert_eq!(invoke_telemetry(&table, handle), Ok(counter));
    }

    #[test]
    fn invoking_an_unknown_handle_is_denied_as_no_such_capability() {
        let table = CapTable::new();
        assert_eq!(
            invoke_telemetry(&table, Handle::from_raw(0)),
            Err(Denied::NoSuchCapability)
        );
    }

    #[test]
    fn invoking_a_stale_handle_is_denied_as_no_such_capability() {
        let (table, handle, _span) = CapTable::bootstrap(StringId(1));
        let stale = Handle::new(handle.index(), handle.generation() + 1);
        assert_eq!(
            invoke_telemetry(&table, stale),
            Err(Denied::NoSuchCapability)
        );
    }

    #[test]
    fn invoking_a_capability_that_lacks_emit_is_denied_for_the_missing_right() {
        let mut table = CapTable::new();
        let handle = table.insert(Capability {
            object: Object::TelemetrySink {
                counter: StringId(1),
            },
            rights: Rights::NONE,
        });
        assert_eq!(invoke_telemetry(&table, handle), Err(Denied::MissingRight));
    }

    #[test]
    fn the_bootstrap_grant_gives_init_a_telemetry_sink_and_a_span_sink() {
        // The "root caps to init only" policy, pinned: init is born with
        // exactly two authorities — emit telemetry and open spans — each with
        // EMIT over its own object. Granting the wrong rights or object here
        // is a privilege bug, so it's host-tested, not left to the itest.
        let counter = StringId(42);
        let (table, telemetry, span) = CapTable::bootstrap(counter);

        let tcap = table
            .resolve(telemetry)
            .expect("the telemetry handle resolves");
        assert_eq!(tcap.object, Object::TelemetrySink { counter });
        assert!(tcap.rights.contains(Rights::EMIT));
        // Telemetry lands in the first (empty) slot; the kernel hands it to
        // the process at startup. The deterministic slot makes this meaningful.
        assert_eq!(telemetry.raw(), 0);

        let scap = table.resolve(span).expect("the span handle resolves");
        assert_eq!(scap.object, Object::SpanSink);
        assert!(scap.rights.contains(Rights::EMIT));
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
            object: Object::TelemetrySink {
                counter: StringId(2),
            },
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
            object: Object::TelemetrySink {
                counter: StringId(1),
            },
            rights: Rights::NONE,
        });

        assert!(table.resolve(full).unwrap().rights.contains(Rights::EMIT));
        assert!(
            !table
                .resolve(attenuated)
                .unwrap()
                .rights
                .contains(Rights::EMIT)
        );
    }

    // --- v0.9b: single-use (`Once`) capabilities ---

    #[test]
    fn consuming_a_capability_makes_its_handle_stale() {
        // The consume step of a single-use capability: invoke once, then the
        // handle (and any copy) no longer resolves.
        let mut table = CapTable::new();
        let h = table.insert_once(emit_sink());
        assert!(table.resolve(h).is_ok());

        assert!(table.consume(h));
        assert_eq!(table.resolve(h), Err(CapError::Stale));
    }

    #[test]
    fn consuming_an_unknown_handle_returns_false() {
        let mut table = CapTable::new();
        assert!(!table.consume(Handle::from_raw(0)));
    }

    #[test]
    fn consuming_a_stale_handle_refuses_and_leaves_the_live_cap() {
        // A stale-generation handle (occupied slot, wrong generation) must NOT
        // consume the live capability now in that slot. Guards the
        // generation-mismatch branch independently of the empty-slot branch.
        let mut table = CapTable::new();
        let valid = table.insert_once(emit_sink());
        let stale = Handle::new(valid.index(), valid.generation() + 1);

        assert!(!table.consume(stale));
        assert!(table.resolve(valid).is_ok());
    }

    #[test]
    fn a_consumed_slot_is_reused_and_old_handles_stay_stale() {
        // The freed slot is reclaimed by the next insert (so a server's table
        // doesn't grow per call), and the generation bump stops the old handle
        // from aliasing the new occupant.
        let mut table = CapTable::new();
        let first = table.insert_once(emit_sink());
        assert!(table.consume(first));

        let second = table.insert(emit_sink());
        assert_eq!(first.index(), second.index()); // same physical slot
        assert_ne!(first, second); // different generation
        assert_eq!(table.resolve(first), Err(CapError::Stale)); // old handle dead
        assert!(table.resolve(second).is_ok()); // new handle live
    }

    #[test]
    fn a_grant_reports_its_multiplicity() {
        // The marker the invoke path reads to decide whether to consume.
        let mut table = CapTable::new();
        let persistent = table.insert(emit_sink());
        let once = table.insert_once(emit_sink());

        assert_eq!(
            table.multiplicity_of(persistent),
            Ok(Multiplicity::Persistent)
        );
        assert_eq!(table.multiplicity_of(once), Ok(Multiplicity::Once));
    }

    #[test]
    fn multiplicity_of_a_consumed_handle_is_stale() {
        let mut table = CapTable::new();
        let once = table.insert_once(emit_sink());
        assert!(table.consume(once));
        assert_eq!(table.multiplicity_of(once), Err(CapError::Stale));
    }

    // --- v0.9b: the reply capability ---

    fn reply_cap(caller: u32) -> Capability {
        Capability {
            object: Object::Reply {
                caller: TaskId(caller),
            },
            rights: Rights::NONE,
        }
    }

    #[test]
    fn invoke_reply_returns_the_caller_to_wake() {
        // Holding a Reply cap *is* the authority to answer that caller — no
        // rights bit; the kernel mints it Once (granted via insert_once).
        let mut table = CapTable::new();
        let h = table.insert_once(reply_cap(7));
        assert_eq!(invoke_reply(&table, h), Ok(TaskId(7)));
    }

    #[test]
    fn invoke_reply_refuses_a_non_reply_object() {
        let mut table = CapTable::new();
        let h = table.insert(Capability {
            object: Object::SpanSink,
            rights: Rights::EMIT,
        });
        assert_eq!(invoke_reply(&table, h), Err(Denied::WrongObject));
    }

    #[test]
    fn invoke_reply_refuses_an_unknown_handle() {
        let table = CapTable::new();
        assert_eq!(
            invoke_reply(&table, Handle::from_raw(0)),
            Err(Denied::NoSuchCapability)
        );
    }

    #[test]
    fn invoke_reply_refuses_a_consumed_handle() {
        // After the cap is consumed (the reply already happened), a second
        // reply through the same handle is refused — single-use enforced.
        let mut table = CapTable::new();
        let h = table.insert_once(reply_cap(7));
        assert!(table.consume(h));
        assert_eq!(invoke_reply(&table, h), Err(Denied::NoSuchCapability));
    }

    // --- v0.11: spawn-with-caps delegation ---

    #[test]
    fn delegate_with_no_handles_yields_no_caps() {
        let table = CapTable::new();
        assert_eq!(delegate(&table, &[]), Ok(Vec::new()));
    }

    #[test]
    fn delegate_resolves_held_handles_in_order() {
        let mut table = CapTable::new();
        let a = table.insert(emit_sink());
        let b = table.insert(Capability {
            object: Object::SpanSink,
            rights: Rights::EMIT,
        });
        assert_eq!(
            delegate(&table, &[a, b]),
            Ok(Vec::from([
                Capability {
                    object: Object::TelemetrySink {
                        counter: StringId(1)
                    },
                    rights: Rights::EMIT
                },
                Capability {
                    object: Object::SpanSink,
                    rights: Rights::EMIT
                },
            ]))
        );
    }

    #[test]
    fn delegate_is_all_or_nothing_when_any_handle_is_unheld() {
        // A bogus handle alongside a valid one fails the whole delegation — no
        // partial child cap set, and you can't conjure a cap you don't hold.
        let mut table = CapTable::new();
        let valid = table.insert(emit_sink());
        let bogus = Handle::from_raw(999);
        assert_eq!(
            delegate(&table, &[valid, bogus]),
            Err(CapError::OutOfBounds)
        );
    }

    #[test]
    fn delegate_rejects_a_stale_handle() {
        let mut table = CapTable::new();
        let h = table.insert_once(emit_sink());
        assert!(table.consume(h));
        assert_eq!(delegate(&table, &[h]), Err(CapError::Stale));
    }

    #[test]
    fn delegate_copies_leaving_the_parents_caps_intact() {
        // Copy, not move: the parent still holds the cap after delegating it
        // (attenuation is mint-then-delegate, never a move of the original).
        let mut table = CapTable::new();
        let h = table.insert(emit_sink());
        let _ = delegate(&table, &[h]).expect("a held handle delegates");
        assert!(table.resolve(h).is_ok());
    }
}
