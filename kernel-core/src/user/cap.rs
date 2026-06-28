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

use snitchos_abi::{object_kind, CapDesc};

use crate::ipc::EndpointId;
use crate::notify::NotificationId;
use crate::sched::TaskId;
// `StringId` was the `TelemetrySink { counter }` payload; the sink is now pure
// authority (Step 5), so no `protocol` type is named here.

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

    /// May `signal` a `Notification` — the producer end (v0.12). Disjoint from
    /// [`WAIT`](Self::WAIT) so a cap can grant either notification end or both,
    /// the same split as [`SEND`](Self::SEND)/[`RECV`](Self::RECV).
    pub const SIGNAL: Rights = Rights(snitchos_abi::rights::SIGNAL);

    /// May `wait` on a `Notification` — the consumer end (v0.12).
    pub const WAIT: Rights = Rights(snitchos_abi::rights::WAIT);

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
    /// Permission to register + emit named metrics (debt #2). Carries no
    /// payload — it is pure authority, exactly like [`SpanSink`](Self::SpanSink):
    /// the holder names its own metrics through `RegisterMetric` (the kernel
    /// interns each into the holder's per-process metric table) and emits through
    /// the returned handle. The legacy `Invoke`-to-a-bound-counter shape (a
    /// `{ counter }` field) was retired once every emitter moved to the register
    /// path.
    TelemetrySink,
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
    /// A notification — the general async kernel→user signal (v0.12). The cap
    /// names *which* notification by [`NotificationId`]; [`Rights::SIGNAL`] /
    /// [`Rights::WAIT`] decide which end the holder may use (producer / consumer),
    /// the same split as [`Endpoint`](Self::Endpoint)'s `SEND`/`RECV`. The signal
    /// payload is one userspace-defined bit mask the kernel only carries. The
    /// object + semantics live in [`crate::notify`].
    Notification { id: NotificationId },
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
    /// This holding's stable global capability id — the derivation-tree node
    /// identity, minted kernel-side and recorded at grant. A transfer of this
    /// cap names it as the child's `parent_cap_id`. `0` is the root/unassigned
    /// sentinel (legacy `insert` without an id), matching the wire convention.
    cap_id: u64,
    /// The `cap_id` of the holding this one was derived from — its parent in the
    /// capability derivation tree (`0` = root / self-created). Stored kernel-side
    /// (not just emitted on the wire) so revocation can walk descendants
    /// transitively. See `docs/cap-revocation-design.md`.
    parent_cap_id: u64,
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

/// Authorize a `TelemetrySink` use: `handle` must name a capability in `table`
/// carrying [`Rights::EMIT`] over an [`Object::TelemetrySink`]. Returns `Ok(())`
/// when the holder may register/emit named metrics, or why it is refused — the
/// `TelemetrySink` twin of [`invoke_span`], now that the sink is pure authority
/// (no bound counter to hand back). `RegisterMetric` gates on this; `EmitMetric`
/// then validates the per-process metric handle, not the cap.
///
/// Pure and host-tested. The kernel side only acts on the result: proceed on
/// `Ok`, snitch + return an error code on `Err`.
pub fn authorize_telemetry(table: &CapTable, handle: Handle) -> Result<(), Denied> {
    let cap = table
        .resolve(handle)
        .map_err(|_| Denied::NoSuchCapability)?;
    if !cap.rights.contains(Rights::EMIT) {
        return Err(Denied::MissingRight);
    }
    let Object::TelemetrySink = cap.object else {
        return Err(Denied::WrongObject);
    };
    Ok(())
}

/// Resolve a `SpanSink` invocation: `handle` must name a capability in
/// `table` carrying [`Rights::EMIT`] over an [`Object::SpanSink`]. Returns
/// `Ok(())` when the holder is authorized to open a span — name, parent, and
/// task id are all supplied kernel-side, so there is nothing to hand back.
/// Pure and host-tested, the span twin of [`authorize_telemetry`].
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
/// [`invoke_recv`]; both mirror [`authorize_telemetry`].
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

/// Resolve a `signal` invocation (v0.12): `handle` must name an
/// [`Object::Notification`] in `table` carrying [`Rights::SIGNAL`] — the
/// producer end. Returns the [`NotificationId`] to signal. The `wait` twin is
/// [`invoke_wait`]; both mirror [`invoke_send`]/[`invoke_recv`].
pub fn invoke_signal(table: &CapTable, handle: Handle) -> Result<NotificationId, Denied> {
    let cap = table
        .resolve(handle)
        .map_err(|_| Denied::NoSuchCapability)?;
    if !cap.rights.contains(Rights::SIGNAL) {
        return Err(Denied::MissingRight);
    }
    let Object::Notification { id } = cap.object else {
        return Err(Denied::WrongObject);
    };
    Ok(id)
}

/// Resolve a `wait` invocation (v0.12): `handle` must name an
/// [`Object::Notification`] in `table` carrying [`Rights::WAIT`] — the consumer
/// end. The mirror of [`invoke_signal`].
pub fn invoke_wait(table: &CapTable, handle: Handle) -> Result<NotificationId, Denied> {
    let cap = table
        .resolve(handle)
        .map_err(|_| Denied::NoSuchCapability)?;
    if !cap.rights.contains(Rights::WAIT) {
        return Err(Denied::MissingRight);
    }
    let Object::Notification { id } = cap.object else {
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

    /// Snapshot the live capabilities in this table as packed [`CapDesc`] records,
    /// in slot order — the kernel-core half of the `CapList` syscall (`hold`). One
    /// entry per live slot: its [`Handle`] (raw), the [`object_kind`] discriminant,
    /// the raw [`Rights`] bits, and the endpoint badge (`0` for non-endpoints).
    /// `reserved` is `0`. This is the "packed hitch" the syscall copies out;
    /// userspace `unhitch`es it into named records. Pure (no kernel state), so it
    /// is host-tested here rather than behind the syscall.
    #[must_use]
    pub fn describe(&self) -> Vec<CapDesc> {
        self.slots
            .iter()
            .enumerate()
            .filter_map(|(index, slot)| {
                let cap = slot.cap.as_ref()?;
                let (kind, badge) = match cap.object {
                    Object::TelemetrySink => (object_kind::TELEMETRY_SINK, 0),
                    Object::SpanSink => (object_kind::SPAN_SINK, 0),
                    Object::Endpoint { badge, .. } => (object_kind::ENDPOINT, badge),
                    Object::Reply { .. } => (object_kind::REPLY, 0),
                    Object::Notification { .. } => (object_kind::NOTIFICATION, 0),
                };
                Some(CapDesc {
                    handle: Handle::new(index as u32, slot.generation).raw(),
                    kind,
                    rights: cap.rights.bits(),
                    reserved: 0,
                    badge,
                })
            })
            .collect()
    }

    /// Build the initial capability table for the `init` process: the two
    /// bootstrap authorities, each with `EMIT` over its own object — an
    /// [`Object::TelemetrySink`] (authority to register + emit named metrics),
    /// then an [`Object::SpanSink`]. Returns the table and the two handles, in
    /// grant order (telemetry in slot 0, span in slot 1). This is the "root caps
    /// to init only" policy — the *only* authority a userspace process is born
    /// with.
    #[must_use]
    pub fn bootstrap() -> (Self, Handle, Handle) {
        Self::bootstrap_with_ids(0, 0)
    }

    /// As [`bootstrap`](Self::bootstrap), but stamps the two holdings with the
    /// global `cap_id`s minted by the kernel — so each bootstrap grant has a
    /// stable derivation-tree identity its `CapEvent::Granted` reports and a
    /// later delegation can name as `parent_cap_id`. The plain `bootstrap` uses
    /// the root sentinel `0` for both (host tests that don't trace ids).
    #[must_use]
    pub fn bootstrap_with_ids(telemetry_id: u64, span_id: u64) -> (Self, Handle, Handle) {
        let mut table = Self::new();
        // Bootstrap caps are roots of the derivation tree → parent 0.
        let telemetry = table.insert_with_id(
            Capability {
                object: Object::TelemetrySink,
                rights: Rights::EMIT,
            },
            telemetry_id,
            0,
        );
        let span = table.insert_with_id(
            Capability {
                object: Object::SpanSink,
                rights: Rights::EMIT,
            },
            span_id,
            0,
        );
        (table, telemetry, span)
    }

    /// Grant `cap` as a [`Multiplicity::Persistent`] capability and return the
    /// handle that names it. Used at bootstrap to grant a process its root
    /// capabilities. The holding gets the root/unassigned cap id (`0`); use
    /// [`insert_with_id`](Self::insert_with_id) to record a real derivation-tree id.
    pub fn insert(&mut self, cap: Capability) -> Handle {
        self.grant(cap, Multiplicity::Persistent, 0, 0)
    }

    /// Grant `cap` as a [`Multiplicity::Persistent`] capability, stamping the
    /// holding with the global `cap_id` (minted kernel-side via `next_cap_id`) —
    /// the derivation-tree node identity a later transfer names as `parent_cap_id`
    /// — and recording `parent_cap_id` (the holding this was derived from; `0` for
    /// a root / self-created grant) so revocation can walk descendants.
    pub fn insert_with_id(&mut self, cap: Capability, cap_id: u64, parent_cap_id: u64) -> Handle {
        self.grant(cap, Multiplicity::Persistent, cap_id, parent_cap_id)
    }

    /// Grant `cap` as a single-use [`Multiplicity::Once`] capability: the first
    /// successful invoke should [`consume`](Self::consume) it. v0.9b's reply
    /// cap is the first user.
    pub fn insert_once(&mut self, cap: Capability) -> Handle {
        self.grant(cap, Multiplicity::Once, 0, 0)
    }

    /// Grant `cap` as a single-use [`Multiplicity::Once`] capability, stamping
    /// the holding with the global `cap_id` + `parent_cap_id`. The id'd twin of
    /// [`insert_once`].
    pub fn insert_once_with_id(
        &mut self,
        cap: Capability,
        cap_id: u64,
        parent_cap_id: u64,
    ) -> Handle {
        self.grant(cap, Multiplicity::Once, cap_id, parent_cap_id)
    }

    /// Place `cap` in the table with `multiplicity` and the global `cap_id`,
    /// reusing a freed slot if one exists (so a server consuming reply caps
    /// doesn't grow the table unboundedly) — otherwise appending. A reused slot
    /// keeps its bumped generation, so handles to its former occupant stay stale;
    /// its `cap_id` is overwritten with the new holding's.
    fn grant(
        &mut self,
        cap: Capability,
        multiplicity: Multiplicity,
        cap_id: u64,
        parent_cap_id: u64,
    ) -> Handle {
        if let Some((index, slot)) = self
            .slots
            .iter_mut()
            .enumerate()
            .find(|(_, s)| s.cap.is_none())
        {
            slot.multiplicity = multiplicity;
            slot.cap = Some(cap);
            slot.cap_id = cap_id;
            slot.parent_cap_id = parent_cap_id;
            return Handle::new(index as u32, slot.generation);
        }
        let index = self.slots.len() as u32;
        let generation = 0; // fresh slot
        self.slots.push(Slot {
            generation,
            multiplicity,
            cap: Some(cap),
            cap_id,
            parent_cap_id,
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

    /// Revoke the live holding whose derivation-tree id is `cap_id`, if present in
    /// this table: free its slot and bump the generation, so the handle (and any
    /// same-table copy of it) now resolves [`CapError::Stale`]. Returns whether a
    /// live holding was revoked. Unlike [`consume`](Self::consume) (which names a
    /// holding by *handle*, the within-table path), this names it by global
    /// `cap_id` — so a revoker can reclaim a grant in *another* process's table
    /// (the kernel scans tables and calls this on each). `cap_id`s are globally
    /// unique per live holding, so at most one slot matches.
    ///
    /// **Non-transitive:** revokes exactly the named holding, not its descendants
    /// (delegated copies carry their own `cap_id`s). Transitive revocation (2T)
    /// will drive a cross-table derivation-tree walk over `parent_cap_id` on top of
    /// this primitive. The root/unassigned sentinel `0` is never a target —
    /// revoking it would hit every legacy/root holding — so `cap_id == 0` is a no-op.
    pub fn revoke_by_cap_id(&mut self, cap_id: u64) -> bool {
        if cap_id == 0 {
            return false;
        }
        let Some(slot) = self
            .slots
            .iter_mut()
            .find(|s| s.cap.is_some() && s.cap_id == cap_id)
        else {
            return false;
        };
        slot.cap = None;
        slot.generation = slot.generation.wrapping_add(1);
        true
    }

    /// The `cap_id`s of the live holdings in this table whose parent is
    /// `parent_cap_id` — its *direct* children in the derivation tree. Transitive
    /// revocation (2T) calls this across every process table to expand the
    /// to-revoke frontier: a revoked node's children are revoked too, then *their*
    /// children, until the frontier stops growing. Returns empty for the root
    /// sentinel `0` (every root holding has parent 0 — not a walkable node), so the
    /// walk can never sweep the whole forest.
    #[must_use]
    pub fn children_cap_ids(&self, parent_cap_id: u64) -> Vec<u64> {
        if parent_cap_id == 0 {
            return Vec::new();
        }
        self.slots
            .iter()
            .filter(|s| s.cap.is_some() && s.parent_cap_id == parent_cap_id)
            .map(|s| s.cap_id)
            .collect()
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

    /// The stable global `cap_id` of the holding `handle` names — its identity
    /// in the capability derivation tree. The kernel reads this at a transfer to
    /// record the source as the child's `parent_cap_id`. Validated like
    /// [`resolve`](Self::resolve): a bad/stale/freed handle is a [`CapError`].
    pub fn cap_id_of(&self, handle: Handle) -> Result<u64, CapError> {
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
        Ok(slot.cap_id)
    }

    /// The `parent_cap_id` of the holding `handle` names — its parent in the
    /// capability derivation tree (`0` = root / self-created). Revocation walks
    /// these links to find descendants. Validated like [`cap_id_of`](Self::cap_id_of).
    pub fn parent_cap_id_of(&self, handle: Handle) -> Result<u64, CapError> {
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
        Ok(slot.parent_cap_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_holding_carries_the_global_cap_id_it_was_inserted_with() {
        // The derivation-tree spine: a capability *holding* (a table slot) is
        // stamped with a stable global cap id at insert, minted kernel-side and
        // passed in, so a later transfer can name it as a `parent_cap_id`.
        let mut table = CapTable::new();
        let handle = table.insert_with_id(
            Capability {
                object: Object::SpanSink,
                rights: Rights::EMIT,
            },
            0x00C0_FFEE,
            0,
        );
        assert_eq!(table.cap_id_of(handle), Ok(0x00C0_FFEE));
    }

    #[test]
    fn distinct_holdings_keep_their_distinct_cap_ids() {
        // The id is per-holding, not a constant or the latest write.
        let mut table = CapTable::new();
        let span = Capability {
            object: Object::SpanSink,
            rights: Rights::EMIT,
        };
        let first = table.insert_with_id(span, 11, 0);
        let second = table.insert_with_id(span, 22, 0);
        assert_eq!(table.cap_id_of(first), Ok(11));
        assert_eq!(table.cap_id_of(second), Ok(22));
    }

    #[test]
    fn a_holding_records_its_parent_cap_id_for_the_derivation_tree() {
        // 2T prework: each holding stores its parent's `cap_id`, so revocation can
        // walk the derivation tree (a delegated cap records the source holding it
        // was derived from). The kernel only emitted this on the wire before.
        let mut table = CapTable::new();
        let cap = Capability { object: Object::TelemetrySink, rights: Rights::EMIT };
        let child = table.insert_with_id(cap, 42, 7);
        assert_eq!(table.cap_id_of(child), Ok(42), "own id");
        assert_eq!(table.parent_cap_id_of(child), Ok(7), "parent id (derivation edge)");
    }

    #[test]
    fn a_root_holding_has_parent_cap_id_zero() {
        // Genuinely-root grants (bootstrap, self-created objects) carry parent 0.
        let mut table = CapTable::new();
        let h = table.insert(Capability { object: Object::SpanSink, rights: Rights::EMIT });
        assert_eq!(table.parent_cap_id_of(h), Ok(0));
    }

    #[test]
    fn revoke_by_cap_id_invalidates_exactly_that_holding() {
        // Revoke-by-id is the powerbox reclaim: name a holding by its derivation-tree
        // id and invalidate it, leaving siblings untouched. The handle then resolves
        // Stale (same as `consume`), and another holding with a different id survives.
        let mut table = CapTable::new();
        let cap = Capability { object: Object::SpanSink, rights: Rights::EMIT };
        let victim = table.insert_with_id(cap, 42, 0);
        let bystander = table.insert_with_id(cap, 43, 0);

        assert!(table.revoke_by_cap_id(42), "a live holding with id 42 was revoked");

        assert_eq!(table.cap_id_of(victim), Err(CapError::Stale), "victim handle now stale");
        assert_eq!(table.cap_id_of(bystander), Ok(43), "the bystander is untouched");
    }

    #[test]
    fn revoke_by_cap_id_is_a_no_op_for_an_absent_or_already_revoked_id() {
        let mut table = CapTable::new();
        let cap = Capability { object: Object::SpanSink, rights: Rights::EMIT };
        table.insert_with_id(cap, 42, 0);

        assert!(!table.revoke_by_cap_id(999), "no holding with id 999");
        assert!(table.revoke_by_cap_id(42), "first revoke frees it");
        assert!(!table.revoke_by_cap_id(42), "second revoke finds nothing live");
    }

    #[test]
    fn children_cap_ids_lists_direct_children_only() {
        // The 2T transitive walk: given a (revoked) parent, find the holdings that
        // derived from it — its *direct* delegated children — so the frontier can
        // expand. Grandchildren come from a later iteration on a child, not here.
        let mut table = CapTable::new();
        let cap = Capability { object: Object::SpanSink, rights: Rights::EMIT };
        table.insert_with_id(cap, 10, 0); // root
        table.insert_with_id(cap, 11, 10); // child of 10
        table.insert_with_id(cap, 12, 10); // child of 10
        table.insert_with_id(cap, 13, 11); // grandchild (child of 11)

        let mut kids = table.children_cap_ids(10);
        kids.sort_unstable();
        assert_eq!(kids, alloc::vec![11, 12], "only the direct children of 10");
        assert_eq!(table.children_cap_ids(11), alloc::vec![13], "13 is a child of 11");
    }

    #[test]
    fn children_cap_ids_excludes_revoked_children_and_the_root_sentinel() {
        let mut table = CapTable::new();
        let cap = Capability { object: Object::SpanSink, rights: Rights::EMIT };
        table.insert_with_id(cap, 10, 0); // root (parent 0)
        table.insert_with_id(cap, 11, 10);
        table.revoke_by_cap_id(11);

        assert!(table.children_cap_ids(10).is_empty(), "a revoked child isn't walkable");
        // Walking from the root sentinel must not sweep every root holding.
        assert!(table.children_cap_ids(0).is_empty(), "root sentinel 0 has no walkable children");
    }

    #[test]
    fn revoke_by_cap_id_refuses_the_root_sentinel_zero() {
        // cap_id 0 is the root/unassigned sentinel shared by every legacy/root
        // holding — revoking "0" must NOT nuke them all. It's never a real target.
        let mut table = CapTable::new();
        let root = table.insert(Capability { object: Object::SpanSink, rights: Rights::EMIT });

        assert!(!table.revoke_by_cap_id(0), "cap_id 0 is not a revocation target");
        assert_eq!(table.cap_id_of(root), Ok(0), "the root holding survives");
    }

    #[test]
    fn the_legacy_insert_leaves_a_holding_at_the_root_sentinel_id() {
        // `insert` (no id) is the root/unassigned `0` — matching the wire's
        // `parent_cap_id: 0` = root convention.
        let mut table = CapTable::new();
        let handle = table.insert(Capability {
            object: Object::SpanSink,
            rights: Rights::EMIT,
        });
        assert_eq!(table.cap_id_of(handle), Ok(0));
    }

    #[test]
    fn bootstrap_with_ids_stamps_each_sink_with_its_own_id() {
        // The telemetry id lands on telemetry, the span id on span — not swapped.
        let (table, telemetry, span) = CapTable::bootstrap_with_ids(7, 9);
        assert_eq!(table.cap_id_of(telemetry), Ok(7));
        assert_eq!(table.cap_id_of(span), Ok(9));
    }

    #[test]
    fn cap_id_of_an_unknown_handle_is_an_error() {
        let table = CapTable::new();
        assert_eq!(
            table.cap_id_of(Handle::from_raw(0)),
            Err(CapError::OutOfBounds)
        );
    }

    #[test]
    fn cap_id_of_a_consumed_handle_is_stale() {
        // A freed holding's id must not be readable through the old handle —
        // the same trust-boundary guard as `resolve`.
        let mut table = CapTable::new();
        let handle = table.insert_once_with_id(
            Capability {
                object: Object::Reply { caller: TaskId(3) },
                rights: Rights::NONE,
            },
            99,
            0,
        );
        assert!(table.consume(handle));
        assert_eq!(table.cap_id_of(handle), Err(CapError::Stale));
    }

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
            object: Object::TelemetrySink,
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
            object: Object::TelemetrySink,
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

    fn notification_cap(id: u32, rights: Rights) -> Capability {
        Capability {
            object: Object::Notification {
                id: crate::notify::NotificationId(id),
            },
            rights,
        }
    }

    #[test]
    fn invoke_signal_accepts_a_notification_with_the_signal_right() {
        let mut table = CapTable::new();
        let h = table.insert(notification_cap(5, Rights::SIGNAL));
        assert_eq!(invoke_signal(&table, h), Ok(crate::notify::NotificationId(5)));
    }

    #[test]
    fn invoke_signal_refuses_a_notification_lacking_the_signal_right() {
        // Holding only the WAIT (consumer) end must not let you signal.
        let mut table = CapTable::new();
        let h = table.insert(notification_cap(5, Rights::WAIT));
        assert_eq!(invoke_signal(&table, h), Err(Denied::MissingRight));
    }

    #[test]
    fn invoke_signal_refuses_an_endpoint_as_wrong_object() {
        let mut table = CapTable::new();
        let h = table.insert(Capability {
            object: Object::Endpoint { id: EndpointId(5), badge: 0 },
            rights: Rights::SIGNAL,
        });
        assert_eq!(invoke_signal(&table, h), Err(Denied::WrongObject));
    }

    #[test]
    fn invoke_signal_refuses_an_unknown_handle() {
        let table = CapTable::new();
        assert_eq!(
            invoke_signal(&table, Handle::from_raw(0)),
            Err(Denied::NoSuchCapability)
        );
    }

    #[test]
    fn invoke_wait_accepts_a_notification_with_the_wait_right() {
        let mut table = CapTable::new();
        let h = table.insert(notification_cap(8, Rights::WAIT));
        assert_eq!(invoke_wait(&table, h), Ok(crate::notify::NotificationId(8)));
    }

    #[test]
    fn invoke_wait_refuses_a_notification_lacking_the_wait_right() {
        // Holding only the SIGNAL (producer) end must not let you wait.
        let mut table = CapTable::new();
        let h = table.insert(notification_cap(8, Rights::SIGNAL));
        assert_eq!(invoke_wait(&table, h), Err(Denied::MissingRight));
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
    fn the_signal_and_wait_rights_are_distinct_disjoint_bits() {
        // The producer (`SIGNAL`) and consumer (`WAIT`) ends of a notification —
        // disjoint so one cap can grant either end or both, like SEND/RECV.
        assert_ne!(Rights::SIGNAL.bits(), Rights::WAIT.bits());
        assert!(!Rights::SIGNAL.contains(Rights::WAIT));
        assert!(!Rights::WAIT.contains(Rights::SIGNAL));
        for other in [Rights::EMIT, Rights::SEND, Rights::RECV, Rights::MINT] {
            assert_ne!(Rights::SIGNAL.bits(), other.bits());
            assert_ne!(Rights::WAIT.bits(), other.bits());
        }
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
            object: Object::TelemetrySink,
            rights: Rights::MINT,
        };
        assert_eq!(
            mint_badged(parent, 0xF00D, Rights::SEND),
            Err(Denied::WrongObject)
        );
    }

    #[test]
    fn authorize_telemetry_refuses_spansink_as_wrong_object() {
        let mut table = CapTable::new();
        let h = table.insert(Capability {
            object: Object::SpanSink,
            rights: Rights::EMIT,
        });
        assert_eq!(authorize_telemetry(&table, h), Err(Denied::WrongObject));
    }

    #[test]
    fn a_resolved_handle_returns_the_capability_that_was_inserted() {
        let mut table = CapTable::new();
        let handle = table.insert(Capability {
            object: Object::TelemetrySink,
            rights: Rights::EMIT,
        });

        let cap = table
            .resolve(handle)
            .expect("a freshly inserted handle resolves");

        assert!(cap.rights.contains(Rights::EMIT));
        assert_eq!(cap.object, Object::TelemetrySink);
    }

    fn emit_sink() -> Capability {
        Capability {
            object: Object::TelemetrySink,
            rights: Rights::EMIT,
        }
    }

    #[test]
    fn describe_lists_live_caps_as_packed_records() {
        let mut table = CapTable::new();
        let telemetry = table.insert(emit_sink());
        let endpoint = table.insert(Capability {
            object: Object::Endpoint { id: EndpointId(7), badge: 0xab },
            rights: Rights::SEND | Rights::MINT,
        });

        let descs = table.describe();

        assert_eq!(
            descs,
            alloc::vec![
                CapDesc {
                    handle: telemetry.raw(),
                    kind: object_kind::TELEMETRY_SINK,
                    rights: snitchos_abi::rights::EMIT,
                    reserved: 0,
                    badge: 0,
                },
                CapDesc {
                    handle: endpoint.raw(),
                    kind: object_kind::ENDPOINT,
                    rights: snitchos_abi::rights::SEND | snitchos_abi::rights::MINT,
                    reserved: 0,
                    badge: 0xab,
                },
            ]
        );
    }

    #[test]
    fn describe_skips_consumed_slots() {
        let mut table = CapTable::new();
        let first = table.insert(emit_sink());
        let second = table.insert(emit_sink());
        assert!(table.consume(first));

        let descs = table.describe();

        assert_eq!(descs.len(), 1);
        assert_eq!(descs[0].handle, second.raw());
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
            object: Object::TelemetrySink,
            rights: Rights::EMIT,
        });

        assert_ne!(first, second);
        assert_eq!(table.resolve(first).unwrap().object, Object::TelemetrySink);
        assert_eq!(table.resolve(second).unwrap().object, Object::TelemetrySink);
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
            object: Object::TelemetrySink,
            rights: Rights::NONE,
        };
        assert!(!cap.rights.contains(Rights::EMIT));
    }

    #[test]
    fn authorizing_a_granted_telemetry_sink_succeeds() {
        // The sink is pure authority now — a granted `TelemetrySink` with `EMIT`
        // authorizes register/emit; there is no bound counter to hand back.
        let (table, handle, _span) = CapTable::bootstrap();

        assert_eq!(authorize_telemetry(&table, handle), Ok(()));
    }

    #[test]
    fn authorizing_an_unknown_handle_is_denied_as_no_such_capability() {
        let table = CapTable::new();
        assert_eq!(
            authorize_telemetry(&table, Handle::from_raw(0)),
            Err(Denied::NoSuchCapability)
        );
    }

    #[test]
    fn authorizing_a_stale_handle_is_denied_as_no_such_capability() {
        let (table, handle, _span) = CapTable::bootstrap();
        let stale = Handle::new(handle.index(), handle.generation() + 1);
        assert_eq!(
            authorize_telemetry(&table, stale),
            Err(Denied::NoSuchCapability)
        );
    }

    #[test]
    fn authorizing_a_capability_that_lacks_emit_is_denied_for_the_missing_right() {
        let mut table = CapTable::new();
        let handle = table.insert(Capability {
            object: Object::TelemetrySink,
            rights: Rights::NONE,
        });
        assert_eq!(authorize_telemetry(&table, handle), Err(Denied::MissingRight));
    }

    #[test]
    fn the_bootstrap_grant_gives_init_a_telemetry_sink_and_a_span_sink() {
        // The "root caps to init only" policy, pinned: init is born with
        // exactly two authorities — emit telemetry and open spans — each with
        // EMIT over its own object. Granting the wrong rights or object here
        // is a privilege bug, so it's host-tested, not left to the itest.
        let (table, telemetry, span) = CapTable::bootstrap();

        let tcap = table
            .resolve(telemetry)
            .expect("the telemetry handle resolves");
        assert_eq!(tcap.object, Object::TelemetrySink);
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
            object: Object::TelemetrySink,
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
            object: Object::TelemetrySink,
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

    #[test]
    fn a_notification_cap_is_held_and_resolved() {
        // The v0.12 notification object: named by a NotificationId, gated by the
        // SIGNAL/WAIT rights. Resolving returns the same object distinct from an
        // endpoint sharing the raw id (the variant, not the number, is identity).
        let mut table = CapTable::new();
        let id = crate::notify::NotificationId(3);
        let handle = table.insert(Capability {
            object: Object::Notification { id },
            rights: Rights::SIGNAL | Rights::WAIT,
        });

        let cap = table.resolve(handle).unwrap();
        assert_eq!(cap.object, Object::Notification { id });
        assert!(cap.rights.contains(Rights::SIGNAL));
        assert!(cap.rights.contains(Rights::WAIT));
        assert_ne!(cap.object, Object::Endpoint { id: EndpointId(3), badge: 0 });
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
                    object: Object::TelemetrySink,
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
