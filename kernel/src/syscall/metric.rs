//! Metric syscalls (debt #2): `RegisterMetric` (cap-gated — needs a
//! `TelemetrySink`) and `EmitMetric` (validated against the caller's *own*
//! per-process metric table). Userspace names its own metrics; the kernel
//! interns each into a **fresh** id and hands back an opaque handle. The
//! per-process table is the forgery boundary — a process can only emit to a
//! metric it registered, never another's or the kernel's own telemetry.
//!
//! The authority gate is the host-tested `kernel_proc::cap::authorize_telemetry`
//! decision (a `TelemetrySink` with `EMIT`) — the sink is pure authority, so
//! there is nothing to hand back beyond "permitted."

use crate::trap::TrapFrame;

/// Map the `a3` metric-kind selector to its wire [`protocol::MetricKind`]. The
/// integers match the runtime's `snitchos_user::MetricKind` discriminants — the
/// single fact both sides agree on. An unknown selector is refused, not coerced.
fn metric_kind_from_usize(n: usize) -> Option<protocol::MetricKind> {
    match n {
        0 => Some(protocol::MetricKind::Counter),
        1 => Some(protocol::MetricKind::Gauge),
        2 => Some(protocol::MetricKind::Histogram),
        _ => None,
    }
}

/// Register a userspace-named metric. `a0` = `TelemetrySink` handle (the gate),
/// `a1` = name pointer, `a2` = name length, `a3` = metric kind. Validates the
/// capability, refuses on a full per-process metric table *before* leaking the
/// name, interns it into a fresh id, stores it in the caller's table, and
/// returns the metric handle in `a0` (or `u64::MAX` on refusal).
pub(super) fn handle_register_metric(frame: &mut TrapFrame) {
    use kernel_proc::cap::{Handle, authorize_telemetry};
    use kernel_mem::mmu::MAX_USER_STR_LEN;
    use protocol::RefusalReason;
    use snitchos_abi::Syscall;

    let sc = Syscall::RegisterMetric as u8;

    let Some(proc) = super::current_process_or_refuse(frame, sc) else {
        return;
    };

    // Authority: the caller must hold a `TelemetrySink` cap (with `EMIT`) at the
    // handle in `a0`. Resolve under the lock, then drop it before interning.
    let denied = {
        let caps = proc.caps.lock();
        authorize_telemetry(&caps, Handle::from_raw(frame.a0 as u32)).err()
    };
    if let Some(d) = denied {
        // A failed cap gate is a capability denial — snitch the rate
        // (`cap.denied_total`) as well as the per-call `SyscallRefused`, exactly
        // as `Invoke`/`MintBadged` do. (The non-cap refusals below — quota, bad
        // range/UTF-8/kind — are not capability denials, so they don't bump it.)
        if let Some(id) = crate::user::cap_denied_metric_id() {
            crate::tracing::emit_metric(id, 1);
        }
        super::refuse(frame, sc, super::refusal_for(d));
        return;
    }

    let Some(kind) = metric_kind_from_usize(frame.a3 as usize) else {
        super::refuse(frame, sc, RefusalReason::BadMetricKind);
        return;
    };

    // Quota: refuse *before* leaking + interning a name if the table is full, so
    // a quota-refused registration commits no permanent `'static` string. (One
    // thread per process, traps run interrupts-masked — no concurrent registrar,
    // so the check-then-register below cannot race itself full.)
    if proc.metrics.lock().is_full() {
        super::refuse(frame, sc, RefusalReason::Quota);
        return;
    }

    // Copy + UTF-8-validate the name out of user memory (range-checked, SUM-guarded).
    let mut buf = [0u8; MAX_USER_STR_LEN];
    let Some(bytes) = crate::user::copy_from_user(frame.a1 as usize, frame.a2 as usize, &mut buf)
    else {
        super::refuse(frame, sc, RefusalReason::BadUserRange);
        return;
    };
    let Ok(name) = core::str::from_utf8(bytes) else {
        super::refuse(frame, sc, RefusalReason::BadUtf8);
        return;
    };

    // Intern into a fresh id (+ `StringRegister`/`MetricRegister` frames), then
    // store it in the caller's table → the handle U-mode emits through.
    let id = crate::tracing::register_user_metric(name, kind);
    let handle = proc
        .metrics
        .lock()
        .register(id)
        .expect("is_full checked above guarantees room");
    frame.a0 = u64::from(handle.raw());
}

/// Emit a sample to a metric the caller registered. `a0` = metric handle (from
/// `RegisterMetric`), `a1` = value. Resolves the handle against the *caller's
/// own* metric table → the bound `StringId` and emits. A handle the caller never
/// registered is refused (`SyscallRefused{BadMetricHandle}`), never silently
/// emitted — possession of a valid handle is the authority.
pub(super) fn handle_emit_metric(frame: &mut TrapFrame) {
    use kernel_proc::metric::MetricHandle;
    use protocol::RefusalReason;
    use snitchos_abi::Syscall;

    let sc = Syscall::EmitMetric as u8;

    let Some(proc) = super::current_process_or_refuse(frame, sc) else {
        return;
    };

    // Resolve under the lock, copy out the id, drop the lock before emitting —
    // never hold a `Mutex` across telemetry emission.
    let resolved = proc.metrics.lock().resolve(MetricHandle::from_raw(frame.a0 as u32));
    match resolved {
        Some(id) => {
            crate::tracing::emit_metric(id, frame.a1 as i64);
            frame.a0 = 0; // success
        }
        None => super::refuse(frame, sc, RefusalReason::BadMetricHandle),
    }
}
