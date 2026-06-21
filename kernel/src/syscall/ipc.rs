//! IPC syscalls: the synchronous rendezvous surface — one-way `Send`/`Receive`
//! and the RPC `Call`/`Reply`/`ReplyRecv` round-trip. The endpoint state machine
//! itself lives in [`crate::ipc`] (the re-export of `crate::trap::ipc`); these
//! handlers — `crate::syscall::ipc` — drive it from the `TrapFrame` and own the
//! reply-cap minting + trace-context seeding.

use crate::trap::TrapFrame;

/// Send an inline message over an IPC endpoint. `a0` = `Endpoint` handle
/// (needs `SEND`), `a1..=a4` = the message words. Resolve the cap against the
/// running process's table; on success drive the rendezvous: either deliver
/// to a waiting receiver and wake it, or block until one arrives. The endpoint
/// lock is dropped inside `ipc::send_begin` before we block/wake (never hold a
/// lock across the switch). Returns `0` on success, the error sentinel if the
/// capability is refused.
pub(super) fn handle_send(frame: &mut TrapFrame) {
    use kernel_core::cap::{invoke_send, Handle};
    use snitchos_abi::Syscall;

    let sc = Syscall::Send as u8;
    let Some(proc) = super::current_process_or_refuse(frame, sc) else {
        return;
    };

    let ep = {
        let caps = proc.caps.lock();
        invoke_send(&caps, Handle::from_raw(frame.a0 as u32))
    };
    let (ep, badge) = match ep {
        Ok(pair) => pair,
        Err(denied) => {
            super::refuse(frame, sc, super::refusal_for(denied));
            return;
        }
    };

    let me = crate::sched::current_task_id();
    let msg = [frame.a1, frame.a2, frame.a3, frame.a4];
    // Carry the sender's trace context: its innermost open span becomes the
    // parent of the receiver's handling span (kernel-populated — userspace can
    // neither forge nor forget it).
    let parent = crate::tracing::current_span_id();
    match crate::ipc::send_begin(ep, me, msg, parent, badge) {
        crate::ipc::SendStep::Deliver { wake } => crate::sched::wake(wake),
        crate::ipc::SendStep::Block => {
            crate::ipc::BLOCKS_TOTAL.inc();
            crate::sched::block_current();
        }
    }
    // Either path completes the rendezvous: the message was (or will be) taken
    // by the receiver. Report success.
    frame.a0 = 0;
}

/// Receive an inline message from an IPC endpoint. `a0` = `Endpoint` handle
/// (needs `RECV`). Drive the rendezvous: take a waiting sender's message (and
/// wake it), or block until one arrives and take the message delivered to us.
/// Writes the words into `a1..=a4` and `0` into `a0`; refuses with the error
/// sentinel if the capability is refused.
pub(super) fn handle_receive(frame: &mut TrapFrame) {
    use kernel_core::cap::{invoke_recv, Handle};
    use snitchos_abi::Syscall;

    let sc = Syscall::Receive as u8;
    let Some(proc) = super::current_process_or_refuse(frame, sc) else {
        return;
    };

    let ep = {
        let caps = proc.caps.lock();
        invoke_recv(&caps, Handle::from_raw(frame.a0 as u32))
    };
    let ep = match ep {
        Ok(ep) => ep,
        Err(denied) => {
            super::refuse(frame, sc, super::refusal_for(denied));
            return;
        }
    };

    receive_into_frame(proc, frame, ep);
}

/// The receive half of `receive`/`reply_recv`: rendezvous on `ep`, mint a
/// reply cap for a `call` (handle into `a5`; `0` for a one-way `send`), seed the
/// sender's trace context, and write the message + status into `frame`. Counts
/// the crossing and snitches the `Message` frame.
fn receive_into_frame(
    proc: &crate::process::Process,
    frame: &mut TrapFrame,
    ep: kernel_core::ipc::EndpointId,
) {
    let me = crate::sched::current_task_id();
    let delivered = match crate::ipc::receive_begin(ep, me) {
        crate::ipc::RecvStep::Deliver { delivered, wake } => {
            // Wake the sender only for a one-way `send`. For a `call` the caller
            // is parked awaiting its reply — we mint it a reply cap instead.
            if delivered.reply_to.is_none() {
                crate::sched::wake(wake);
            }
            delivered
        }
        crate::ipc::RecvStep::Block => {
            crate::ipc::BLOCKS_TOTAL.inc();
            crate::sched::block_current();
            // Resumed: a sender stashed our message under our id at rendezvous.
            crate::ipc::take_delivered(ep, me)
        }
    };

    // If this was a `call`, mint a one-shot reply cap into *this* (server)
    // process naming the caller, and hand its handle back in `a5`; a one-way
    // `send` yields `a5 = 0`.
    frame.a5 = reply_handle_for(proc, me, delivered.reply_to);

    // Deliver the sender cap's badge in `a6` (v0.9c) — the unforgeable demux
    // value the receiver uses to tell its objects/clients apart. `0` for a bare
    // (unbadged) cap.
    frame.a6 = delivered.badge;

    // The message crossed. Count it, and record the topology + trace link on
    // the wire — both at delivery, outside the endpoint critical section.
    crate::ipc::MESSAGES_TOTAL.inc();
    crate::tracing::emit_message(ep.0, delivered.from.0, me.0, delivered.parent);
    // Seed the sender's span as our incoming parent — the next span this task
    // opens (its handling span) becomes a child, so the trace continues across
    // the process boundary.
    crate::tracing::set_current_parent(delivered.parent);
    frame.a1 = delivered.msg[0];
    frame.a2 = delivered.msg[1];
    frame.a3 = delivered.msg[2];
    frame.a4 = delivered.msg[3];
    frame.a0 = 0;
}

/// Mint a one-shot reply cap into `proc` (the receiving server) naming the
/// blocked `caller`, snitch it as `CapEvent::Transferred`, and return its raw
/// handle for `receive`'s `a5`. Returns `0` when `reply_to` is `None` (a
/// one-way `send` — no reply expected). The first cross-process cap insertion:
/// the kernel grants the server authority to answer exactly this caller once.
fn reply_handle_for(
    proc: &crate::process::Process,
    holder: kernel_core::sched::TaskId,
    reply_to: Option<kernel_core::sched::TaskId>,
) -> u64 {
    use kernel_core::cap::{Capability, Object, Rights};

    let Some(caller) = reply_to else {
        return 0;
    };
    let handle = proc
        .caps
        .lock()
        .insert_once(Capability { object: Object::Reply { caller }, rights: Rights::NONE });
    crate::tracing::emit_cap_transferred(
        crate::process::next_cap_id(),
        holder.0,
        protocol::CapObject::Reply,
        Rights::NONE.bits(),
        0, // reply caps carry no badge
    );
    u64::from(handle.raw())
}

/// RPC `call`: `a0` = `Endpoint` handle (needs `SEND`), `a1..=a4` = request
/// words. Delivers the request (marked as a call so the receiver mints a reply
/// cap), then parks the caller until `reply` wakes it — at which point the
/// response words are written into `a1..=a4`. The caller's span stays open
/// across the round-trip, so the server's handling span nests under it.
pub(super) fn handle_call(frame: &mut TrapFrame) {
    use kernel_core::cap::{invoke_send, Handle};
    use snitchos_abi::Syscall;

    let sc = Syscall::Call as u8;
    let Some(proc) = super::current_process_or_refuse(frame, sc) else {
        return;
    };

    let ep = {
        let caps = proc.caps.lock();
        invoke_send(&caps, Handle::from_raw(frame.a0 as u32))
    };
    let (ep, badge) = match ep {
        Ok(pair) => pair,
        Err(denied) => {
            super::refuse(frame, sc, super::refusal_for(denied));
            return;
        }
    };

    crate::ipc::CALLS_TOTAL.inc();
    let me = crate::sched::current_task_id();
    let req = [frame.a1, frame.a2, frame.a3, frame.a4];
    let parent = crate::tracing::current_span_id();
    match crate::ipc::call_begin(ep, me, req, parent, badge) {
        crate::ipc::SendStep::Deliver { wake } => crate::sched::wake(wake),
        crate::ipc::SendStep::Block => {}
    }
    // The caller always parks awaiting the reply — woken by `reply`, never by
    // the request rendezvous (a receiver taking a call mints a reply cap
    // instead of waking us).
    crate::ipc::BLOCKS_TOTAL.inc();
    crate::sched::block_current();

    // Resumed by `reply`: deliver the response words and, if the server handed
    // us a capability, insert it into *our* table (we're the current process
    // again) and return its handle in `a5` (`0` = no cap). v0.9c.
    let reply = crate::ipc::take_reply(me);
    frame.a1 = reply.msg[0];
    frame.a2 = reply.msg[1];
    frame.a3 = reply.msg[2];
    frame.a4 = reply.msg[3];
    frame.a5 = match reply.cap {
        Some(cap) => {
            let handle = proc.caps.lock().insert(cap);
            let badge = match cap.object {
                kernel_core::cap::Object::Endpoint { badge, .. } => badge,
                _ => 0,
            };
            crate::tracing::emit_cap_transferred(
                crate::process::next_cap_id(),
                me.0,
                protocol::CapObject::Endpoint,
                cap.rights.bits(),
                badge,
            );
            u64::from(handle.raw())
        }
        None => 0,
    };
    frame.a0 = 0;
}

/// RPC `reply`: `a0` = reply-cap handle (from `receive`'s `a5`), `a1..=a4` =
/// response words. Resolves + **consumes** the one-shot reply cap (a second
/// `reply` is refused), stashes the response for the blocked caller, and wakes
/// it. The reply is point-to-point (server→caller), not endpoint-mediated.
pub(super) fn handle_reply(frame: &mut TrapFrame) {
    use snitchos_abi::Syscall;

    let sc = Syscall::Reply as u8;
    let Some(proc) = super::current_process_or_refuse(frame, sc) else {
        return;
    };
    let raw_handle = frame.a0 as u32;
    let resp = [frame.a1, frame.a2, frame.a3, frame.a4];
    // `a6` (v0.9c): a cap handle to transfer to the caller, `0` = none.
    let transfer = (frame.a6 != 0).then(|| kernel_core::cap::Handle::from_raw(frame.a6 as u32));
    if reply_via_cap(proc, frame, sc, raw_handle, resp, transfer).is_ok() {
        frame.a0 = 0;
    }
}

/// The reply half of `reply`/`reply_recv`: resolve + **consume** the one-shot
/// reply cap `raw_handle` against `proc`, stash `resp` for the named caller, and
/// wake it. `Err(())` (with `refuse` already emitted) if the handle is not a
/// live reply cap. Resolve+consume under the caps lock; drop it before waking
/// (never hold a `Mutex` across the path that may switch).
fn reply_via_cap(
    proc: &crate::process::Process,
    frame: &mut TrapFrame,
    sc: u8,
    raw_handle: u32,
    resp: crate::ipc::Message,
    transfer: Option<kernel_core::cap::Handle>,
) -> Result<(), ()> {
    use kernel_core::cap::{invoke_reply, Handle};

    let handle = Handle::from_raw(raw_handle);
    // Resolve + consume the reply cap and, if the server is handing a cap to the
    // caller, *move* it out of the server's table (resolve, copy out, consume) —
    // all under one lock, dropped before the wake. A transfer handle that names
    // no cap is silently no-op (the caller simply receives no cap).
    let resolved = {
        let mut caps = proc.caps.lock();
        match invoke_reply(&caps, handle) {
            Err(denied) => Err(denied),
            Ok(caller) => {
                caps.consume(handle);
                let cap = match transfer {
                    Some(h) => {
                        let c = caps.resolve(h).copied().ok();
                        if c.is_some() {
                            caps.consume(h);
                        }
                        c
                    }
                    None => None,
                };
                Ok((caller, cap))
            }
        }
    };
    let (caller, cap) = match resolved {
        Ok(pair) => pair,
        Err(denied) => {
            super::refuse(frame, sc, super::refusal_for(denied));
            return Err(());
        }
    };
    crate::ipc::stash_reply(caller, crate::ipc::StashedReply { msg: resp, cap });
    crate::sched::wake(caller);
    crate::ipc::REPLIES_TOTAL.inc();
    Ok(())
}

/// Fused `reply`-then-`receive` (the RPC server hot path): `a0` = `Endpoint`
/// handle, `a5` = the previous request's reply handle (`0` = none, first
/// iteration), `a1..=a4` = the response to it. Replies the previous caller (if
/// any), then runs a normal receive into `frame` for the next request. One trap
/// instead of two; reuses [`reply_via_cap`] + [`receive_into_frame`].
pub(super) fn handle_reply_recv(frame: &mut TrapFrame) {
    use kernel_core::cap::{invoke_recv, Handle};
    use snitchos_abi::Syscall;

    let sc = Syscall::ReplyRecv as u8;
    let Some(proc) = super::current_process_or_refuse(frame, sc) else {
        return;
    };

    // Reply half — skipped on the first iteration (no previous request).
    let prev_reply = frame.a5 as u32;
    if prev_reply != 0 {
        let resp = [frame.a1, frame.a2, frame.a3, frame.a4];
        // reply_recv does not carry a transferred cap in v0.9c (the plain `reply`
        // path does); pass `None`. Fusing cap-transfer into the hot path is a
        // deferred additive step.
        if reply_via_cap(proc, frame, sc, prev_reply, resp, None).is_err() {
            return; // refused — `a0` already set, don't receive
        }
    }

    // Receive half — block for the next request on the endpoint in `a0`.
    let ep = {
        let caps = proc.caps.lock();
        invoke_recv(&caps, Handle::from_raw(frame.a0 as u32))
    };
    let ep = match ep {
        Ok(ep) => ep,
        Err(denied) => {
            super::refuse(frame, sc, super::refusal_for(denied));
            return;
        }
    };
    receive_into_frame(proc, frame, ep);
}
