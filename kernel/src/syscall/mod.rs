//! U-mode `ecall` dispatch and the syscall handlers, one module per call type.
//!
//! [`handle_user_ecall`] is the syscall demux reached from the trap entry in
//! [`crate::trap`]: `a7` selects the syscall and one handler module below
//! implements it. The authority decisions themselves are the pure, host-tested
//! `kernel_core::cap` functions; these handlers only marshal the [`TrapFrame`]
//! registers and act on the result. The shared refusal / process-resolution
//! helpers live here (accessible to the child modules); the trap/IRQ entry
//! machinery (`TrapFrame` layout, timer, CSR setup) lives in [`crate::trap`].

mod cap;
mod clock;
mod console;
mod debug;
mod ipc;
mod mem;
mod metric;
mod process;
mod span;
mod transfer;

use core::sync::atomic::Ordering;

use crate::trap::TrapFrame;

/// Handle an `ecall` from U-mode. The kernel surface is **invoke a capability**
/// plus a handful of ambient ops: `a7` selects the syscall, `a0` is the handle
/// into the *calling process's* `CapTable`, `a1..` the arguments. Each arm
/// dispatches to its call-type module; the handler resolves and rights-checks
/// against that table (no ambient authority for cap-mediated ops), then we
/// advance `sepc` past the `ecall`.
pub(crate) fn handle_user_ecall(frame: &mut TrapFrame) {
    use snitchos_abi::Syscall;
    match Syscall::from_usize(frame.a7 as usize) {
        Some(Syscall::Exit) => process::handle_exit(frame), // does not return
        Some(Syscall::Yield) => crate::sched::yield_now(),
        Some(Syscall::SpanOpen) => span::handle_span_open(frame),
        Some(Syscall::SpanClose) => span::handle_span_close(frame),
        Some(Syscall::MapAnon) => mem::handle_map_anon(frame),
        Some(Syscall::DebugWrite) => debug::handle_debug_write(frame),
        Some(Syscall::Send) => ipc::handle_send(frame),
        Some(Syscall::Receive) => ipc::handle_receive(frame),
        Some(Syscall::Call) => ipc::handle_call(frame),
        Some(Syscall::Reply) => ipc::handle_reply(frame),
        Some(Syscall::ReplyRecv) => ipc::handle_reply_recv(frame),
        Some(Syscall::MintBadged) => cap::handle_mint_badged(frame),
        Some(Syscall::CopyFromCaller) => transfer::handle_copy_from_caller(frame),
        Some(Syscall::CopyToCaller) => transfer::handle_copy_to_caller(frame),
        Some(Syscall::ConsoleRead) => console::handle_console_read(frame),
        Some(Syscall::ConsoleWrite) => console::handle_console_write(frame),
        Some(Syscall::ClockNow) => clock::handle_clock_now(frame),
        Some(Syscall::Spawn) => process::handle_spawn(frame),
        Some(Syscall::Wait) => process::handle_wait(frame),
        Some(Syscall::RegisterMetric) => metric::handle_register_metric(frame),
        Some(Syscall::EmitMetric) => metric::handle_emit_metric(frame),
        None => {
            let n = frame.a7 as u8;
            refuse(frame, n, protocol::RefusalReason::UnknownSyscall);
        }
    }
    // `ecall` is a 4-byte instruction; without advancing past it, `sret`
    // would re-execute it and we'd trap on it forever. (Not reached for
    // `Exit` — `handle_exit` never returns.)
    frame.sepc = frame.sepc.wrapping_add(4);
}

/// Refuse a syscall: snitch a `SyscallRefused` event (so the denial is never
/// silent) and return the error sentinel in `a0`. Private to this module tree;
/// every handler module reaches it via `super::refuse`.
fn refuse(frame: &mut TrapFrame, syscall: u8, reason: protocol::RefusalReason) {
    crate::tracing::emit_syscall_refused(syscall, reason);
    frame.a0 = u64::MAX;
}

/// Map a capability-invocation denial to its wire refusal reason.
fn refusal_for(denied: kernel_core::cap::Denied) -> protocol::RefusalReason {
    use kernel_core::cap::Denied;
    use protocol::RefusalReason;
    match denied {
        Denied::NoSuchCapability => RefusalReason::CapNotFound,
        Denied::MissingRight => RefusalReason::CapWrongRights,
        Denied::WrongObject => RefusalReason::CapWrongObject,
    }
}

/// Resolve the user process running on this hart, or — if none — snitch a
/// `NoProcess` refusal (setting the error sentinel in `a0`) and return `None`
/// for the caller to early-return. Every capability syscall opens with this:
/// it needs the calling process's `CapTable`.
fn current_process_or_refuse(
    frame: &mut TrapFrame,
    syscall: u8,
) -> Option<&'static crate::process::Process> {
    let proc = crate::process::CURRENT_PROCESS.this_cpu().load(Ordering::Relaxed);
    // SAFETY: set by `user::run` on this hart before `sret`; the `Process` lives
    // in that never-returning frame, so a non-null pointer is valid for the
    // life of the kernel. Null only if no user process runs here — which then
    // could not have issued this U-mode `ecall`.
    match unsafe { proc.as_ref() } {
        Some(proc) => Some(proc),
        None => {
            refuse(frame, syscall, protocol::RefusalReason::NoProcess);
            None
        }
    }
}
