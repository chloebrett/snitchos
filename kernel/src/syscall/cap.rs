//! Capability syscalls: `MintBadged` — derive a narrower (badged) capability.
//! Resolves a handle against the calling process's `CapTable` and acts on the
//! pure, host-tested `kernel_core::cap` decision.
//!
//! (The legacy `Invoke` syscall — emit to a `TelemetrySink`'s bound counter —
//! was retired in debt #2's Step 5: telemetry now flows through
//! `RegisterMetric`/`EmitMetric`, and its ABI number was removed/renumbered.)

use crate::trap::TrapFrame;

/// Mint a badged `SEND` capability for an endpoint the caller owns (v0.9c).
/// `a0` = endpoint handle (needs `MINT`), `a1` = the server-chosen `badge`,
/// `a2` = requested rights. Resolve the parent against the running process's
/// table, derive the child via the pure host-tested
/// [`kernel_core::cap::mint_badged`], insert it into the caller's *own* table,
/// and return its handle in `a0` (or refuse with `a0 = u64::MAX`). Snitched as
/// `CapEvent::Transferred` carrying the badge. Handing the cap to a client is a
/// later step; here it lands in the minter's table.
pub(super) fn handle_mint_badged(frame: &mut TrapFrame) {
    use kernel_core::cap::{mint_badged, Denied, Handle, Rights};
    use snitchos_abi::Syscall;

    let sc = Syscall::MintBadged as u8;
    let Some(proc) = super::current_process_or_refuse(frame, sc) else {
        return;
    };

    let handle = Handle::from_raw(frame.a0 as u32);
    let badge = frame.a1;
    let rights = Rights::from_bits(frame.a2 as u32);

    // The minted child gets its own global cap id (stamped on the holding, so a
    // later delegation links to it); it derives from the parent endpoint cap, whose
    // id becomes the child's `parent_cap_id` — the derivation edge.
    let child_cap_id = crate::process::next_cap_id();

    // Resolve the parent (copy it out to release the borrow), derive the pure
    // child cap, then insert it. No lock held across a switch — minting cannot
    // block.
    let minted = {
        let mut caps = proc.caps.lock();
        let parent = caps.resolve(handle).copied().map_err(|_| Denied::NoSuchCapability);
        let parent_cap_id = caps.cap_id_of(handle).unwrap_or(0);
        parent
            .and_then(|p| mint_badged(p, badge, rights))
            .map(|child| (caps.insert_with_id(child, child_cap_id, parent_cap_id), parent_cap_id))
    };

    match minted {
        Ok((h, parent_cap_id)) => {
            crate::tracing::emit_cap_transferred(
                child_cap_id,
                parent_cap_id,
                crate::sched::current_task_id().0,
                protocol::CapObject::Endpoint,
                rights.bits(),
                badge,
            );
            frame.a0 = u64::from(h.raw());
        }
        Err(denied) => {
            if let Some(id) = crate::user::cap_denied_metric_id() {
                crate::tracing::emit_metric(id, 1);
            }
            super::refuse(frame, sc, super::refusal_for(denied));
        }
    }
}

/// Enumerate the caller's **own** capability table (`hold`). `a0` = pointer to a
/// `[CapDesc; N]` buffer in the caller's space, `a1` = `N` (capacity in entries).
/// Snapshots the live caps via the pure host-tested [`kernel_core::cap::CapTable::describe`],
/// writes up to `N` packed [`CapDesc`] records out, and returns the **total** live
/// count in `a0` (so a too-small buffer is detectable: returned `>` `N`), or refuses
/// with `a0 = u64::MAX` on a bad/unwritable range. Introspection, not authority — so
/// it is ungated (no cap argument), like `ConsoleRead`/`ClockNow`.
pub(super) fn handle_cap_list(frame: &mut TrapFrame) {
    use protocol::RefusalReason;
    use snitchos_abi::Syscall;

    let sc = Syscall::CapList as u8;
    let Some(proc) = super::current_process_or_refuse(frame, sc) else {
        return;
    };

    let ptr = frame.a0 as usize;
    let capacity = frame.a1 as usize;

    // Snapshot to an owned Vec; the lock drops at the `;` (never held across the
    // copy). `describe` is non-destructive, so validation can ride on
    // `copy_to_user` rather than pre-checking.
    let descs = proc.caps.lock().describe(crate::ipc::name_of);
    let total = descs.len();
    let n = total.min(capacity);

    // The packed-hitch payload: the first `n` entries' bytes, via the one audited
    // cast. `CapDesc: Pod` (derive-checked: `repr(C)`, no padding, all scalars), so
    // the byte image is exactly what userspace `unhitch`es and no uninitialized
    // padding is exposed — guaranteed by the type, not a hand-written `SAFETY`.
    let bytes = hitch_pod::pod_bytes(&descs[..n]);

    match crate::user::copy_to_user(ptr, bytes) {
        Some(_) => frame.a0 = total as u64,
        None => super::refuse(frame, sc, RefusalReason::BadUserRange),
    }
}

/// Revoke the capabilities **derived from** the holding `a0` (a [`Handle`]) names —
/// the powerbox reclaim. Authority is implicit: resolving the handle in the
/// caller's own table proves it holds the cap, which *is* the right to reclaim what
/// was delegated from it (no separate ancestry check). The caller's own holding
/// survives; each transitive descendant is invalidated across every process table
/// (the 2T walk lives in [`crate::sched::revoke_descendants_of`]) and snitched as a
/// `CapEvent::Revoked`. Returns the count revoked in `a0`, or refuses
/// (`a0 = u64::MAX`) if the handle resolves nothing.
pub(super) fn handle_revoke(frame: &mut TrapFrame) {
    use kernel_core::cap::{Handle, Object};
    use protocol::RefusalReason;
    use snitchos_abi::Syscall;

    let sc = Syscall::Revoke as u8;
    let Some(proc) = super::current_process_or_refuse(frame, sc) else {
        return;
    };

    let handle = Handle::from_raw(frame.a0 as u32);
    // Resolve the handle to its `cap_id` (the derivation-tree root to reclaim from),
    // then drop the lock — the walk locks *every* process's caps, including this one.
    let root_cap_id = {
        let caps = proc.caps.lock();
        match caps.cap_id_of(handle) {
            Ok(id) => id,
            Err(_) => {
                super::refuse(frame, sc, RefusalReason::CapNotFound);
                return;
            }
        }
    };

    let revoked = crate::sched::revoke_descendants_of(root_cap_id);
    for (holder, cap_id, parent_cap_id, cap) in &revoked {
        let badge = match cap.object {
            Object::Endpoint { badge, .. } => badge,
            _ => 0,
        };
        crate::tracing::emit_cap_revoked(
            *cap_id,
            *parent_cap_id,
            *holder,
            crate::user::cap_object_kind(cap.object),
            cap.rights.bits(),
            badge,
        );
    }
    frame.a0 = revoked.len() as u64;
}
