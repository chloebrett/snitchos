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

    // Resolve the parent (copy it out to release the borrow), derive the pure
    // child cap, then insert it. No lock held across a switch — minting cannot
    // block.
    let minted = {
        let mut caps = proc.caps.lock();
        let parent = caps.resolve(handle).copied().map_err(|_| Denied::NoSuchCapability);
        parent.and_then(|p| mint_badged(p, badge, rights)).map(|child| caps.insert(child))
    };

    match minted {
        Ok(h) => {
            crate::tracing::emit_cap_transferred(
                crate::process::next_cap_id(),
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
