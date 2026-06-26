//! Span syscalls: `SpanOpen` (cap-gated — needs a `SpanSink`) and `SpanClose`
//! (ambient — validated against the caller's own cursor). The kernel copies +
//! interns the span name and threads the per-task span cursor; userspace never
//! touches the intern table directly.

use crate::trap::TrapFrame;

/// Open a span on behalf of U-mode. `a0` = `SpanSink` handle, `a1` = name
/// pointer, `a2` = name length. Validates the capability, copies + interns the
/// name, opens a span on the calling task's cursor, and returns the span id in
/// `a0` (or `u64::MAX` on refusal).
pub(super) fn handle_span_open(frame: &mut TrapFrame) {
    use kernel_core::cap::{Handle, invoke_span};
    use kernel_core::mmu::MAX_USER_STR_LEN;
    use protocol::RefusalReason;
    use snitchos_abi::Syscall;

    let sc = Syscall::SpanOpen as u8;

    let Some(proc) = super::current_process_or_refuse(frame, sc) else {
        return;
    };

    // Authority: the caller must hold a `SpanSink` cap at handle `a0`. Resolve
    // under the lock, then drop it before the intern/emit path.
    let denied = {
        let caps = proc.caps.lock();
        invoke_span(&caps, Handle::from_raw(frame.a0 as u32)).err()
    };
    if let Some(d) = denied {
        super::refuse(frame, sc, super::refusal_for(d));
        return;
    }

    // Copy the span name out of user memory (range-validated, SUM-guarded).
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

    // Intern on demand (quota-gated, scoped to this process's own span-name
    // table) + open a span on this task's cursor; hand back the `{id, parent}`
    // close token (id in a0, parent in a1). A new name past the per-process quota
    // is refused without leaking.
    let Some(opened) = crate::tracing::span_open_bounded(name, &proc.span_names) else {
        super::refuse(frame, sc, RefusalReason::Quota);
        return;
    };
    frame.a0 = opened.id.0;
    frame.a1 = opened.parent.0;
}

/// Close a span on behalf of U-mode. `a0` = span id, `a1` = parent id (the
/// token `SpanOpen` returned). The kernel validates the id is the caller's
/// innermost open span (cursor top), refusing an out-of-order/forged close.
pub(super) fn handle_span_close(frame: &mut TrapFrame) {
    use protocol::{RefusalReason, SpanId};
    use snitchos_abi::Syscall;

    let id = SpanId(frame.a0);
    let parent = SpanId(frame.a1);
    if crate::tracing::span_close_checked(id, parent) {
        frame.a0 = 0; // success
    } else {
        super::refuse(frame, Syscall::SpanClose as u8, RefusalReason::BadSpanId);
    }
}
