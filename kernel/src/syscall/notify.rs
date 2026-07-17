//! Notification syscalls (v0.12): `NotifyCreate`, `Signal`, `WaitNotify` — the
//! general async kernel→user signal. The authority decisions are the pure,
//! host-tested `kernel_proc::cap::{invoke_signal, invoke_wait}`; the registry +
//! park/unpark live in `crate::sched` (the `NOTIFY` table behind the same `Mutex`
//! discipline as the runqueue). These handlers only marshal the `TrapFrame` and
//! the `block_current` loop. See `docs/notification-design.md`.

use crate::trap::TrapFrame;

/// Create a fresh notification, return a `SIGNAL | WAIT` capability to it (v0.12).
/// Ambient like `MapAnon`: making your own notification needs no prior authority;
/// the caller then attenuates + delegates the end(s) it wants. Snitched as
/// `CapEvent::Minted` (self-minted-via-syscall provenance).
pub(super) fn handle_notify_create(frame: &mut TrapFrame) {
    use kernel_proc::cap::{Capability, Object, Rights};
    use snitchos_abi::Syscall;

    let sc = Syscall::NotifyCreate as u8;
    let Some(proc) = super::current_process_or_refuse(frame, sc) else {
        return;
    };

    let id = crate::sched::notify_create();
    let rights = Rights::SIGNAL | Rights::WAIT;
    // Stamp the holding with its global cap id so a later delegation of an end
    // (e.g. `notify-waiter` handing the `SIGNAL` end to its child) can name it as
    // `parent_cap_id` — the stored id must equal the one this `Granted` reports.
    let cap_id = crate::process::next_cap_id();
    let handle = proc.caps.lock().insert_with_id(
        Capability {
            object: Object::Notification { id },
            rights,
        },
        cap_id,
        0, // self-created notification: a derivation-tree root
    );

    crate::tracing::emit_cap_minted(
        cap_id,
        crate::sched::current_task_id().0,
        protocol::CapObject::Notification,
        rights.bits(),
        [0; snitchos_abi::CAP_NAME_LEN], // notifications carry no name
    );
    frame.a0 = u64::from(handle.raw());
}

/// Signal a notification — the producer end (v0.12). `a0` = handle (needs
/// `SIGNAL`), `a1` = bit mask. OR-s the mask in and wakes any waiter; never
/// blocks. `a0 = 0` on success, `usize::MAX` if refused.
pub(super) fn handle_signal(frame: &mut TrapFrame) {
    use kernel_proc::cap::{invoke_signal, Handle};
    use snitchos_abi::Syscall;

    let sc = Syscall::Signal as u8;
    let Some(proc) = super::current_process_or_refuse(frame, sc) else {
        return;
    };

    let handle = Handle::from_raw(frame.a0 as u32);
    let mask = frame.a1;
    let resolved = invoke_signal(&proc.caps.lock(), handle);
    match resolved {
        Ok(id) => {
            crate::sched::notify_signal(id, mask);
            crate::tracing::emit_notify_signal(id.0, mask, crate::sched::current_task_id().0);
            frame.a0 = 0;
        }
        Err(denied) => super::refuse(frame, sc, super::refusal_for(denied)),
    }
}

/// Wait on a notification — the consumer end (v0.12, timed in v2b). `a0` = handle
/// (needs `WAIT`); `a1` = absolute-tick **deadline** (`0` = block forever, backward
/// compatible). Returns the pending bits in `a0` (read-and-cleared) with `a1 = 0`,
/// blocking until a `Signal` arrives — or, if the deadline passes first, returns
/// `a0 = 0, a1 = 1` (**timed out**). Refuses a bad cap or a second waiter (one waiter
/// per notification).
pub(super) fn handle_wait_notify(frame: &mut TrapFrame) {
    use kernel_proc::cap::{invoke_wait, Handle};
    use kernel_proc::notify::WaitStep;
    use protocol::RefusalReason;
    use snitchos_abi::Syscall;

    let sc = Syscall::WaitNotify as u8;
    let Some(proc) = super::current_process_or_refuse(frame, sc) else {
        return;
    };

    let handle = Handle::from_raw(frame.a0 as u32);
    let id = match invoke_wait(&proc.caps.lock(), handle) {
        Ok(id) => id,
        Err(denied) => {
            super::refuse(frame, sc, super::refusal_for(denied));
            return;
        }
    };

    let deadline = frame.a1;
    let me = crate::sched::current_task_id();

    // Fast path: bits already pending → return at once; otherwise register as the
    // single waiter (or refuse a second waiter / unknown id).
    match crate::sched::notify_wait(id, me) {
        Some(WaitStep::Ready(bits)) => {
            crate::tracing::emit_notify_wait(id.0, bits, me.0);
            frame.a0 = bits;
            frame.a1 = 0;
            return;
        }
        Some(WaitStep::Block) => {} // registered as the waiter — fall through to park
        Some(WaitStep::Busy) => {
            super::refuse(frame, sc, RefusalReason::NotificationBusy);
            return;
        }
        None => {
            super::refuse(frame, sc, RefusalReason::CapNotFound);
            return;
        }
    }

    // Arm the timeout (no-op if `deadline == 0`) and park. Each wake is a `Signal`
    // (bits pending) or the timeout drain (no bits) — `notify_take_pending`
    // distinguishes them without disturbing our waiter registration.
    crate::sched::timeout_register(deadline, me);
    loop {
        crate::sched::block_current();
        if let Some(bits) = crate::sched::notify_take_pending(id) {
            crate::sched::timeout_cancel(me);
            crate::tracing::emit_notify_wait(id.0, bits, me.0);
            frame.a0 = bits;
            frame.a1 = 0;
            return;
        }
        if deadline != 0 && crate::tracing::timestamp() >= deadline {
            // Timed out: free our waiter slot + timeout entry, report it.
            crate::sched::notify_cancel_wait(id, me);
            crate::sched::timeout_cancel(me);
            frame.a0 = 0;
            frame.a1 = 1;
            return;
        }
        // Woken with no bits and not yet timed out (idle-return / early wake): we're
        // still the registered waiter — park again.
    }
}
