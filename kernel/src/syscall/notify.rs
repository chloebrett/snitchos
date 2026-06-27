//! Notification syscalls (v0.12): `NotifyCreate`, `Signal`, `WaitNotify` — the
//! general async kernel→user signal. The authority decisions are the pure,
//! host-tested `kernel_core::cap::{invoke_signal, invoke_wait}`; the registry +
//! park/unpark live in `crate::sched` (the `NOTIFY` table behind the same `Mutex`
//! discipline as the runqueue). These handlers only marshal the `TrapFrame` and
//! the `block_current` loop. See `docs/notification-design.md`.

use crate::trap::TrapFrame;

/// Create a fresh notification, return a `SIGNAL | WAIT` capability to it (v0.12).
/// Ambient like `MapAnon`: making your own notification needs no prior authority;
/// the caller then attenuates + delegates the end(s) it wants. Snitched as
/// `CapEvent::Granted`.
pub(super) fn handle_notify_create(frame: &mut TrapFrame) {
    use kernel_core::cap::{Capability, Object, Rights};
    use snitchos_abi::Syscall;

    let sc = Syscall::NotifyCreate as u8;
    let Some(proc) = super::current_process_or_refuse(frame, sc) else {
        return;
    };

    let id = crate::sched::notify_create();
    let rights = Rights::SIGNAL | Rights::WAIT;
    let handle = proc.caps.lock().insert(Capability {
        object: Object::Notification { id },
        rights,
    });

    crate::tracing::emit_cap_granted(
        crate::process::next_cap_id(),
        crate::sched::current_task_id().0,
        protocol::CapObject::Notification,
        rights.bits(),
    );
    frame.a0 = u64::from(handle.raw());
}

/// Signal a notification — the producer end (v0.12). `a0` = handle (needs
/// `SIGNAL`), `a1` = bit mask. OR-s the mask in and wakes any waiter; never
/// blocks. `a0 = 0` on success, `usize::MAX` if refused.
pub(super) fn handle_signal(frame: &mut TrapFrame) {
    use kernel_core::cap::{invoke_signal, Handle};
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
            frame.a0 = 0;
        }
        Err(denied) => super::refuse(frame, sc, super::refusal_for(denied)),
    }
}

/// Wait on a notification — the consumer end (v0.12). `a0` = handle (needs
/// `WAIT`). Returns the pending bits in `a0` (read-and-cleared), blocking until a
/// `Signal` arrives if none are pending. Refuses a bad cap or a second waiter
/// (one waiter per notification).
pub(super) fn handle_wait_notify(frame: &mut TrapFrame) {
    use kernel_core::cap::{invoke_wait, Handle};
    use kernel_core::notify::WaitStep;
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

    let me = crate::sched::current_task_id();
    loop {
        match crate::sched::notify_wait(id, me) {
            Some(WaitStep::Ready(bits)) => {
                frame.a0 = bits;
                return;
            }
            // Parked as the waiter; block until a `Signal` wakes us, then re-check
            // (the bits the signaller OR-ed in are now pending → `Ready`).
            Some(WaitStep::Block) => crate::sched::block_current(),
            Some(WaitStep::Busy) => {
                super::refuse(frame, sc, RefusalReason::NotificationBusy);
                return;
            }
            // Unknown id behind a valid cap — a kernel-side bug; refuse loudly.
            None => {
                super::refuse(frame, sc, RefusalReason::CapNotFound);
                return;
            }
        }
    }
}
