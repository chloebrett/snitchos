//! Kernel-side synchronous IPC endpoints (v0.9).
//!
//! The pure rendezvous state machine lives in [`kernel_core::ipc`]; this
//! module owns the *table* of endpoints, the in-flight message storage, and
//! wires `send`/`receive` to [`crate::sched`]'s `block_current`/`wake`.
//! Mirrors how [`crate::sched`] owns the task table over
//! `kernel_core::sched::Runqueue`.
//!
//! Lock discipline: every endpoint-state and `pending` access happens under
//! the `ENDPOINTS` lock; the *decision* (whom to wake, whether to block) is
//! computed under the lock and the lock is dropped before `block_current`/
//! `wake` run (never hold a `Mutex` across the switch — see CLAUDE.md). See
//! `plans/v0.9-ipc.md`.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::sync::atomic::AtomicU64;

use kernel_core::ipc::{on_receive, on_send, EndpointId, EndpointState, RendezvousAction};
use kernel_core::sched::TaskId;
use protocol::SpanId;

use crate::sync::{Mutex, Once};

/// Rendezvous count: bumped once per message delivered (the receiver side of a
/// crossing). Drained as `snitchos.ipc.messages_total` in the heartbeat —
/// deferred-emission, never a frame from the rendezvous itself. `Relaxed`: a
/// counter.
pub static MESSAGES_TOTAL: AtomicU64 = AtomicU64::new(0);

/// Block count: bumped once each time a `send`/`receive` parks its caller for
/// want of a peer. Drained as `snitchos.ipc.blocks_total`. `Relaxed`: counter.
pub static BLOCKS_TOTAL: AtomicU64 = AtomicU64::new(0);

/// RPC `call` count: bumped once per accepted `call`. Drained as
/// `snitchos.ipc.calls_total`. `Relaxed`: counter.
pub static CALLS_TOTAL: AtomicU64 = AtomicU64::new(0);

/// RPC `reply` count: bumped once per successful `reply` (a consumed reply cap).
/// Drained as `snitchos.ipc.replies_total`. `Relaxed`: counter.
pub static REPLIES_TOTAL: AtomicU64 = AtomicU64::new(0);

/// Inline message size: the four words carried in `a1..=a4`. Re-exported from
/// `snitchos_abi`, the single source of truth shared with userspace + `fs-proto`.
pub use snitchos_abi::MSG_WORDS;
/// An inline IPC message — the words copied sender→receiver.
pub type Message = [u64; MSG_WORDS];

/// A delivered message + its metadata, handed to the receiver. `parent` is the
/// sender's innermost open span at send time; the kernel seeds it onto the
/// receiver so its handling span becomes a child — the trace following the
/// message across the process boundary (`SpanId(0)` = no context). `reply_to`
/// is `Some(caller)` when the message came from a `call` (the receiver mints a
/// one-shot reply cap for `caller`), `None` for a one-way `send`.
#[derive(Clone, Copy)]
pub struct Delivered {
    pub msg: Message,
    pub parent: SpanId,
    /// The sending task — for the `Message` frame's `from`.
    pub from: TaskId,
    pub reply_to: Option<TaskId>,
    /// The badge of the sender's endpoint cap (v0.9c), delivered to the receiver
    /// so it can demux which object/client this message names. `0` = a bare
    /// (unbadged) endpoint cap.
    pub badge: u64,
}

impl Default for Delivered {
    fn default() -> Self {
        Self { msg: [0; MSG_WORDS], parent: SpanId(0), from: TaskId(0), reply_to: None, badge: 0 }
    }
}

/// One kernel endpoint: the pure rendezvous state plus the in-flight messages
/// of currently-blocked tasks, keyed by the **blocked** task's id. Both fields
/// are touched only under [`ENDPOINTS`].
struct Endpoint {
    state: EndpointState,
    pending: BTreeMap<TaskId, Delivered>,
}

impl Endpoint {
    fn new() -> Self {
        Self { state: EndpointState::Idle, pending: BTreeMap::new() }
    }
}

/// The endpoint table. `EndpointId` indexes it; entries are append-only
/// (no endpoint destruction in v0.9).
static ENDPOINTS: Mutex<Vec<Endpoint>> = Mutex::new(Vec::new());

/// The single kernel-brokered endpoint shared by the `workload=ipc` demo
/// processes. Created once at boot; both processes are bootstrapped with a
/// capability naming it (sender `SEND`, receiver `RECV`).
pub static DEMO_ENDPOINT: Once<EndpointId> = Once::new();

/// Create a fresh endpoint and return its id.
pub fn create() -> EndpointId {
    let mut eps = ENDPOINTS.lock();
    let id = EndpointId(eps.len() as u32);
    eps.push(Endpoint::new());
    id
}

/// What the `send` trap handler must do once the endpoint lock is dropped.
pub enum SendStep {
    /// A receiver was waiting; the message is staged for it — wake it.
    Deliver { wake: TaskId },
    /// No receiver; the message is stashed under the caller's id — block.
    Block,
}

/// What the `receive` trap handler must do once the endpoint lock is dropped.
pub enum RecvStep {
    /// A sender/caller was waiting; `delivered` carries its payload + metadata,
    /// `wake` is the sender to wake **iff** it was a one-way `send`
    /// (`delivered.reply_to == None`). For a `call` the caller is not woken
    /// here — it awaits the `reply` — and the receiver mints a reply cap.
    Deliver { delivered: Delivered, wake: TaskId },
    /// No sender; block until one rendezvouses.
    Block,
}

/// Begin a one-way `send` (`reply_to = None`). See [`begin`].
pub fn send_begin(ep: EndpointId, me: TaskId, msg: Message, parent: SpanId, badge: u64) -> SendStep {
    begin(ep, me, msg, parent, None, badge)
}

/// Begin an RPC `call` (`reply_to = Some(me)`): identical request delivery to a
/// `send`, but the receiver will mint a reply cap and the caller blocks
/// awaiting the reply rather than being woken at the rendezvous.
pub fn call_begin(ep: EndpointId, me: TaskId, msg: Message, parent: SpanId, badge: u64) -> SendStep {
    begin(ep, me, msg, parent, Some(me), badge)
}

/// Drive the pure rendezvous and stage the message: deliver to a waiting
/// receiver (report whom to wake) or stash it under `me` to block on. All under
/// the endpoint lock; the handler acts after it drops. `badge` is the sender
/// cap's badge, delivered to the receiver for demux.
fn begin(ep: EndpointId, me: TaskId, msg: Message, parent: SpanId, reply_to: Option<TaskId>, badge: u64) -> SendStep {
    let mut eps = ENDPOINTS.lock();
    let endpoint = &mut eps[ep.0 as usize];
    let state = core::mem::replace(&mut endpoint.state, EndpointState::Idle);
    let (next, action) = on_send(state, me);
    endpoint.state = next;
    let delivered = Delivered { msg, parent, from: me, reply_to, badge };
    match action {
        RendezvousAction::Rendezvous { peer } => {
            // Deliver to the blocked receiver's slot; it reads this on resume.
            endpoint.pending.insert(peer, delivered);
            SendStep::Deliver { wake: peer }
        }
        RendezvousAction::Block => {
            // Stash my message; a future receiver takes it at rendezvous.
            endpoint.pending.insert(me, delivered);
            SendStep::Block
        }
    }
}

/// Begin a `receive`: drive the pure rendezvous. If a sender/caller was
/// waiting, take its stashed message + metadata and report whom to wake;
/// otherwise block.
pub fn receive_begin(ep: EndpointId, me: TaskId) -> RecvStep {
    let mut eps = ENDPOINTS.lock();
    let endpoint = &mut eps[ep.0 as usize];
    let state = core::mem::replace(&mut endpoint.state, EndpointState::Idle);
    let (next, action) = on_receive(state, me);
    endpoint.state = next;
    match action {
        RendezvousAction::Rendezvous { peer } => {
            let delivered = endpoint.pending.remove(&peer).unwrap_or_default();
            RecvStep::Deliver { delivered, wake: peer }
        }
        RendezvousAction::Block => RecvStep::Block,
    }
}

/// Take the message delivered to `me` while it was blocked in `receive`. Call
/// once, after `block_current` returns. Defaults to an empty `Delivered` if
/// (impossibly) absent — never panics.
pub fn take_delivered(ep: EndpointId, me: TaskId) -> Delivered {
    let mut eps = ENDPOINTS.lock();
    eps[ep.0 as usize].pending.remove(&me).unwrap_or_default()
}

/// A stashed reply: the response words plus an optional capability the server
/// transferred to the caller (v0.9c — `reply`-carries-a-cap). The cap was moved
/// out of the server's table and is inserted into the caller's table when it
/// resumes; `None` is a plain v0.9b reply.
#[derive(Clone, Copy)]
pub struct StashedReply {
    pub msg: Message,
    pub cap: Option<kernel_core::cap::Capability>,
}

/// Point-to-point reply mailbox, keyed by the **caller** awaiting a reply. The
/// `reply` path stashes the response here and wakes the caller; the caller's
/// blocked `call` reads it on resume. Separate from endpoint `pending` because
/// a reply is caller↔server, not endpoint-mediated (the caller is already off
/// the endpoint by the time it awaits the reply).
static REPLIES: Mutex<BTreeMap<TaskId, StashedReply>> = Mutex::new(BTreeMap::new());

/// Stash a `reply` for `caller` (called by `reply` before waking it).
pub fn stash_reply(caller: TaskId, reply: StashedReply) {
    REPLIES.lock().insert(caller, reply);
}

/// Take the reply delivered to `me` (called by `call` after `block_current`
/// returns). Defaults to empty (zeros, no cap) if absent — never panics.
pub fn take_reply(me: TaskId) -> StashedReply {
    REPLIES.lock().remove(&me).unwrap_or(StashedReply { msg: [0; MSG_WORDS], cap: None })
}
