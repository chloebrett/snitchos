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

use kernel_core::ipc::{on_receive, on_send, EndpointId, EndpointState, RendezvousAction};
use kernel_core::sched::TaskId;
use protocol::SpanId;

use crate::sync::{Mutex, Once};

/// Inline message size: the four words carried in `a1..=a4`.
pub const MSG_WORDS: usize = 4;
/// An inline IPC message — the words copied sender→receiver.
pub type Message = [u64; MSG_WORDS];

/// A message in flight, plus the sender's trace context. `parent` is the
/// sender's innermost open span at send time; the kernel seeds it onto the
/// receiver so the receiver's handling span becomes a child — the trace
/// following the message across the process boundary. `SpanId(0)` = no context.
#[derive(Clone, Copy)]
struct Pending {
    msg: Message,
    parent: SpanId,
}

impl Default for Pending {
    fn default() -> Self {
        Self { msg: [0; MSG_WORDS], parent: SpanId(0) }
    }
}

/// One kernel endpoint: the pure rendezvous state plus the in-flight messages
/// of currently-blocked tasks, keyed by the **blocked** task's id. Both fields
/// are touched only under [`ENDPOINTS`].
struct Endpoint {
    state: EndpointState,
    pending: BTreeMap<TaskId, Pending>,
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
    /// A sender was waiting; `msg` is its payload (write it into the receiver's
    /// frame) and `parent` its trace context (seed it onto the receiver) —
    /// wake the sender.
    Deliver { msg: Message, parent: SpanId, wake: TaskId },
    /// No sender; block until one rendezvouses.
    Block,
}

/// Begin a `send`: drive the pure rendezvous, then either stage the message
/// (with the sender's `parent` trace context) for a waiting receiver and
/// report whom to wake, or stash it under `me` for the caller to block on. All
/// under the endpoint lock; the handler acts after it drops.
pub fn send_begin(ep: EndpointId, me: TaskId, msg: Message, parent: SpanId) -> SendStep {
    let mut eps = ENDPOINTS.lock();
    let endpoint = &mut eps[ep.0 as usize];
    let state = core::mem::replace(&mut endpoint.state, EndpointState::Idle);
    let (next, action) = on_send(state, me);
    endpoint.state = next;
    let pending = Pending { msg, parent };
    match action {
        RendezvousAction::Rendezvous { peer } => {
            // Deliver to the blocked receiver's slot; it reads this on resume.
            endpoint.pending.insert(peer, pending);
            SendStep::Deliver { wake: peer }
        }
        RendezvousAction::Block => {
            // Stash my message; a future receiver takes it at rendezvous.
            endpoint.pending.insert(me, pending);
            SendStep::Block
        }
    }
}

/// Begin a `receive`: drive the pure rendezvous. If a sender was waiting, take
/// its stashed message + trace context and report whom to wake; otherwise
/// block.
pub fn receive_begin(ep: EndpointId, me: TaskId) -> RecvStep {
    let mut eps = ENDPOINTS.lock();
    let endpoint = &mut eps[ep.0 as usize];
    let state = core::mem::replace(&mut endpoint.state, EndpointState::Idle);
    let (next, action) = on_receive(state, me);
    endpoint.state = next;
    match action {
        RendezvousAction::Rendezvous { peer } => {
            let Pending { msg, parent } = endpoint.pending.remove(&peer).unwrap_or_default();
            RecvStep::Deliver { msg, parent, wake: peer }
        }
        RendezvousAction::Block => RecvStep::Block,
    }
}

/// Take the message + trace context delivered to `me` while it was blocked in
/// `receive`. Call once, after `block_current` returns: a sender stored it
/// under `me`'s id at rendezvous. Defaults to zeros / no parent if (impossibly)
/// absent — never panics.
pub fn take_delivered(ep: EndpointId, me: TaskId) -> (Message, SpanId) {
    let mut eps = ENDPOINTS.lock();
    let Pending { msg, parent } = eps[ep.0 as usize].pending.remove(&me).unwrap_or_default();
    (msg, parent)
}
