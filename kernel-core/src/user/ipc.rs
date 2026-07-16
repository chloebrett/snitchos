//! Synchronous endpoint rendezvous — the pure v0.9 IPC core.
//!
//! An endpoint is a meeting point with a wait queue. Its invariant: it
//! never holds senders *and* receivers at once — the instant both exist
//! they rendezvous and both proceed. So at rest it is in exactly one of
//! three states. [`on_send`] / [`on_receive`] are the mirror-image
//! transition functions: pure, host-tested, no kernel state. The kernel
//! side owns the table of endpoints, the parked messages, and the
//! block/wake wiring; this module owns only the bookkeeping and its
//! invariant. Mirrors `heap::watermark_grow_decision` /
//! `sched::quantum_expired`.
//!
//! See `plans/v0.9-ipc.md`.

use alloc::collections::VecDeque;

use crate::sched::TaskId;

/// Identifies an endpoint within the kernel's endpoint table. A capability
/// of [`crate::cap::Object::Endpoint`] names one; the kernel resolves it to
/// a slot. kernel-core treats it as an opaque newtype, like [`TaskId`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct EndpointId(pub u32);

/// An endpoint at rest. The invariant — never both sides waiting — is
/// upheld by [`on_send`] / [`on_receive`]: senders only accumulate while
/// no receiver waits, and the first receiver drains a sender (and vice
/// versa). The waiting queues are never empty: draining the last waiter
/// collapses the endpoint back to `Idle`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum EndpointState {
    /// Nobody waiting. An endpoint is born here, so it is the default.
    #[default]
    Idle,
    /// Senders blocked, waiting for a receiver. FIFO: the front sender
    /// rendezvouses with the next receiver to arrive.
    SendersWaiting(VecDeque<TaskId>),
    /// Receivers blocked, waiting for a sender. FIFO likewise.
    ReceiversWaiting(VecDeque<TaskId>),
}

/// What the caller of a `send`/`receive` must do as a result of the
/// transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RendezvousAction {
    /// No peer was waiting: the caller is now parked on the endpoint and
    /// must block (give up the CPU).
    Block,
    /// A peer was waiting: pair with `peer` (copy the message across and
    /// wake it). Neither side stays blocked.
    Rendezvous { peer: TaskId },
}

/// Pop the front waiter from a non-empty queue, collapsing the endpoint
/// back to `Idle` when that was the last one. `rebuild` reconstructs the
/// same waiting variant around the remainder.
fn drain_front(
    mut queue: VecDeque<TaskId>,
    rebuild: impl FnOnce(VecDeque<TaskId>) -> EndpointState,
) -> (EndpointState, TaskId) {
    // SAFETY (logic, not memory): a waiting queue is never empty by
    // construction — `on_send`/`on_receive` collapse to `Idle` the moment
    // the last waiter drains, so this variant only ever holds ≥1 waiter.
    let peer = queue.pop_front().expect("a waiting queue is never empty by construction");
    let next = if queue.is_empty() { EndpointState::Idle } else { rebuild(queue) };
    (next, peer)
}

/// Transition for a sender arriving at the endpoint: rendezvous with a
/// waiting receiver if one exists, otherwise park behind any other
/// senders and block.
#[must_use]
pub fn on_send(state: EndpointState, me: TaskId) -> (EndpointState, RendezvousAction) {
    match state {
        EndpointState::ReceiversWaiting(queue) => {
            let (next, peer) = drain_front(queue, EndpointState::ReceiversWaiting);
            (next, RendezvousAction::Rendezvous { peer })
        }
        EndpointState::Idle => {
            (EndpointState::SendersWaiting(VecDeque::from([me])), RendezvousAction::Block)
        }
        EndpointState::SendersWaiting(mut queue) => {
            queue.push_back(me);
            (EndpointState::SendersWaiting(queue), RendezvousAction::Block)
        }
    }
}

/// Transition for a waiter **leaving** the endpoint without rendezvousing — the
/// v2a `Kill` extracting a blocked task's id from whichever queue holds it, so no
/// ghost id lingers to be popped by a future rendezvous. Removes `me` from the
/// waiting queue (idempotent — a no-op if `me` isn't waiting here), collapsing to
/// `Idle` when it was the last waiter (upholding the never-empty-queue invariant).
/// Unlike [`on_send`]/[`on_receive`] it never rendezvouses, so there's no action to
/// return — just the next state.
#[must_use]
pub fn on_cancel(state: EndpointState, me: TaskId) -> EndpointState {
    match state {
        EndpointState::Idle => EndpointState::Idle,
        EndpointState::SendersWaiting(queue) => {
            without(queue, me, EndpointState::SendersWaiting)
        }
        EndpointState::ReceiversWaiting(queue) => {
            without(queue, me, EndpointState::ReceiversWaiting)
        }
    }
}

/// Drop every occurrence of `me` from `queue`, collapsing to `Idle` when nothing
/// remains; otherwise `rebuild` the same waiting variant around the survivors.
fn without(
    mut queue: VecDeque<TaskId>,
    me: TaskId,
    rebuild: impl FnOnce(VecDeque<TaskId>) -> EndpointState,
) -> EndpointState {
    queue.retain(|&id| id != me);
    if queue.is_empty() {
        EndpointState::Idle
    } else {
        rebuild(queue)
    }
}

/// Transition for a receiver arriving at the endpoint. The mirror of
/// [`on_send`].
#[must_use]
pub fn on_receive(state: EndpointState, me: TaskId) -> (EndpointState, RendezvousAction) {
    match state {
        EndpointState::SendersWaiting(queue) => {
            let (next, peer) = drain_front(queue, EndpointState::SendersWaiting);
            (next, RendezvousAction::Rendezvous { peer })
        }
        EndpointState::Idle => {
            (EndpointState::ReceiversWaiting(VecDeque::from([me])), RendezvousAction::Block)
        }
        EndpointState::ReceiversWaiting(mut queue) => {
            queue.push_back(me);
            (EndpointState::ReceiversWaiting(queue), RendezvousAction::Block)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn senders(ids: &[u32]) -> EndpointState {
        EndpointState::SendersWaiting(ids.iter().copied().map(TaskId).collect())
    }

    fn receivers(ids: &[u32]) -> EndpointState {
        EndpointState::ReceiversWaiting(ids.iter().copied().map(TaskId).collect())
    }

    #[test]
    fn an_endpoint_is_born_idle() {
        assert_eq!(EndpointState::default(), EndpointState::Idle);
    }

    #[test]
    fn send_into_idle_blocks_and_parks_the_sender() {
        let (state, action) = on_send(EndpointState::Idle, TaskId(7));

        assert_eq!(action, RendezvousAction::Block);
        assert_eq!(state, senders(&[7]));
    }

    #[test]
    fn receive_into_idle_blocks_and_parks_the_receiver() {
        let (state, action) = on_receive(EndpointState::Idle, TaskId(7));

        assert_eq!(action, RendezvousAction::Block);
        assert_eq!(state, receivers(&[7]));
    }

    #[test]
    fn receive_with_one_sender_waiting_rendezvouses_and_collapses_to_idle() {
        let (state, action) = on_receive(senders(&[3]), TaskId(9));

        assert_eq!(action, RendezvousAction::Rendezvous { peer: TaskId(3) });
        assert_eq!(state, EndpointState::Idle);
    }

    #[test]
    fn send_with_one_receiver_waiting_rendezvouses_and_collapses_to_idle() {
        let (state, action) = on_send(receivers(&[3]), TaskId(9));

        assert_eq!(action, RendezvousAction::Rendezvous { peer: TaskId(3) });
        assert_eq!(state, EndpointState::Idle);
    }

    #[test]
    fn a_second_sender_queues_behind_the_first() {
        let (state, action) = on_send(senders(&[1]), TaskId(2));

        assert_eq!(action, RendezvousAction::Block);
        assert_eq!(state, senders(&[1, 2]));
    }

    #[test]
    fn a_second_receiver_queues_behind_the_first() {
        let (state, action) = on_receive(receivers(&[1]), TaskId(2));

        assert_eq!(action, RendezvousAction::Block);
        assert_eq!(state, receivers(&[1, 2]));
    }

    #[test]
    fn senders_rendezvous_in_fifo_arrival_order() {
        let (state, _) = on_send(EndpointState::Idle, TaskId(1));
        let (state, _) = on_send(state, TaskId(2));

        let (state, first) = on_receive(state, TaskId(10));
        assert_eq!(first, RendezvousAction::Rendezvous { peer: TaskId(1) });
        assert_eq!(state, senders(&[2]));

        let (state, second) = on_receive(state, TaskId(11));
        assert_eq!(second, RendezvousAction::Rendezvous { peer: TaskId(2) });
        assert_eq!(state, EndpointState::Idle);
    }

    #[test]
    fn a_send_into_receivers_never_produces_a_both_sides_waiting_state() {
        // The invariant: a sender arriving while receivers wait drains a
        // receiver and never appends itself alongside them.
        let (state, action) = on_send(receivers(&[1, 2]), TaskId(99));

        assert_eq!(action, RendezvousAction::Rendezvous { peer: TaskId(1) });
        assert_eq!(state, receivers(&[2]));
    }

    #[test]
    fn a_receive_into_senders_never_produces_a_both_sides_waiting_state() {
        let (state, action) = on_receive(senders(&[1, 2]), TaskId(99));

        assert_eq!(action, RendezvousAction::Rendezvous { peer: TaskId(1) });
        assert_eq!(state, senders(&[2]));
    }

    #[test]
    fn cancelling_the_only_sender_collapses_to_idle() {
        // A killed task parked as the lone sender leaves no waiter — the endpoint
        // returns to Idle, not an empty `SendersWaiting` (the invariant: a waiting
        // queue is never empty).
        assert_eq!(on_cancel(senders(&[3]), TaskId(3)), EndpointState::Idle);
    }

    #[test]
    fn cancelling_the_only_receiver_collapses_to_idle() {
        assert_eq!(on_cancel(receivers(&[3]), TaskId(3)), EndpointState::Idle);
    }

    #[test]
    fn cancelling_one_of_several_senders_keeps_the_rest_in_fifo_order() {
        // Killing the middle sender extracts just its id; the others keep their
        // arrival order so the next receiver still rendezvouses with the front.
        assert_eq!(on_cancel(senders(&[1, 2, 3]), TaskId(2)), senders(&[1, 3]));
    }

    #[test]
    fn cancelling_a_task_that_is_not_waiting_is_a_no_op() {
        // The target blocked elsewhere (another endpoint, notify, reap) — this
        // endpoint's queue is untouched. Idempotent, so a blind scan-all-endpoints
        // cancel is safe.
        assert_eq!(on_cancel(senders(&[1, 2]), TaskId(99)), senders(&[1, 2]));
    }

    #[test]
    fn cancelling_on_an_idle_endpoint_stays_idle() {
        assert_eq!(on_cancel(EndpointState::Idle, TaskId(7)), EndpointState::Idle);
    }
}
