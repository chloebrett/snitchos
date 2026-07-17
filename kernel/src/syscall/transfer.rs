//! Cross-address-space bulk-copy syscalls (v0.10 option D): `CopyFromCaller`
//! (pull, the `write`/`create` half) and `CopyToCaller` (push, the `read` half).
//! The authority is a **borrowed** reply cap — a server may touch only a caller
//! that is parked awaiting *its* reply. The kernel stays object-ignorant: it
//! copies opaque bytes between two page tables through the linear map.

use crate::trap::TrapFrame;

/// Resolve a reply-cap handle in `proc`'s table to the address-space root of the
/// blocked caller it names — **without consuming** the cap (the server may copy
/// before it replies). `None` if the handle isn't a live `Reply` cap or the
/// caller has no user space. The authority check for the cross-AS copy: a server
/// may only touch a caller that is awaiting *its* reply.
fn caller_root_from_reply(proc: &crate::process::Process, handle: kernel_proc::cap::Handle) -> Option<usize> {
    use kernel_proc::cap::Object;
    let caller = {
        let caps = proc.caps.lock();
        match caps.resolve(handle).map(|cap| cap.object) {
            Ok(Object::Reply { caller }) => caller,
            _ => return None,
        }
    };
    crate::sched::address_space_of(caller)
}

/// Copy bytes from a blocked caller's address space into the server's own
/// (v0.10 option D). `a0` = a reply-cap handle the server holds (names the
/// blocked caller — the authority; **borrowed, not consumed**), `a1` = source
/// VA in the caller's space, `a2` = length, `a3` = destination VA in the
/// server's space. Returns bytes copied in `a0`, or snitches a refusal
/// (`a0 = usize::MAX`) on a bad cap / pointer / range. The `write`/`create` half.
pub(super) fn handle_copy_from_caller(frame: &mut TrapFrame) {
    use kernel_proc::cap::Handle;
    use snitchos_abi::Syscall;

    let sc = Syscall::CopyFromCaller as u8;
    let Some(proc) = super::current_process_or_refuse(frame, sc) else {
        return;
    };
    let Some(caller_root) = caller_root_from_reply(proc, Handle::from_raw(frame.a0 as u32)) else {
        super::refuse(frame, sc, protocol::RefusalReason::CapWrongObject);
        return;
    };
    // src = caller's `a1`, dst = this server's `a3`.
    match crate::mmu::copy_across(caller_root, frame.a1 as usize, proc.root_pa, frame.a3 as usize, frame.a2 as usize) {
        Ok(n) => frame.a0 = n as u64,
        Err(_) => super::refuse(frame, sc, protocol::RefusalReason::BadUserRange),
    }
}

/// Copy bytes from the server's address space into a blocked caller's (v0.10
/// option D) — the mirror of [`handle_copy_from_caller`]. `a1` = source VA in
/// the server's space, `a3` = destination VA in the caller's. The `read` half.
pub(super) fn handle_copy_to_caller(frame: &mut TrapFrame) {
    use kernel_proc::cap::Handle;
    use snitchos_abi::Syscall;

    let sc = Syscall::CopyToCaller as u8;
    let Some(proc) = super::current_process_or_refuse(frame, sc) else {
        return;
    };
    let Some(caller_root) = caller_root_from_reply(proc, Handle::from_raw(frame.a0 as u32)) else {
        super::refuse(frame, sc, protocol::RefusalReason::CapWrongObject);
        return;
    };
    // src = this server's `a1`, dst = caller's `a3`.
    match crate::mmu::copy_across(proc.root_pa, frame.a1 as usize, caller_root, frame.a3 as usize, frame.a2 as usize) {
        Ok(n) => frame.a0 = n as u64,
        Err(_) => super::refuse(frame, sc, protocol::RefusalReason::BadUserRange),
    }
}
