//! Process-lifecycle syscalls: `Exit` and `Spawn`.

use core::sync::atomic::Ordering;

use crate::trap::TrapFrame;

/// Terminate the calling user process. Snitches `snitchos.user.exits_total`,
/// clears this hart's current-process pointer (the process is gone), then
/// hands the hart to its next ready task via `sched::exit_now` — which never
/// returns. On the userspace workload that next task is `hart_1_main`, whose
/// idle loop `wfi`s, so the hart goes truly idle rather than busy-spinning.
/// v0.7b leaks the address space + caps; reclamation is a later milestone.
pub(super) fn handle_exit() -> ! {
    if let Some(id) = crate::user::user_exits_metric_id() {
        crate::tracing::emit_metric(id, 1);
    }
    crate::process::CURRENT_PROCESS
        .this_cpu()
        .store(core::ptr::null_mut(), Ordering::Relaxed);
    crate::sched::exit_now()
}

/// Spawn a new userspace process, delegating a subset of the caller's caps to it
/// (v0.11). `a0` = program id, `a1` = pointer to a `[u32; N]` handle array in the
/// caller's space, `a2` = `N`. Resolves the program, delegates the named caps —
/// **all-or-nothing**, refusing the whole spawn if any handle is unheld — builds
/// the child with those caps plus bootstrap telemetry/span, and returns the
/// child's task id in `a0` (or `usize::MAX` on refusal). Ambient like `Yield`,
/// but a process can only delegate authority it already holds.
pub(super) fn handle_spawn(frame: &mut TrapFrame) {
    use kernel_core::cap::Handle;
    use protocol::RefusalReason;
    use snitchos_abi::Syscall;

    let sc = Syscall::Spawn as u8;

    // The caller's table is what we delegate *from*.
    let Some(proc) = super::current_process_or_refuse(frame, sc) else {
        return;
    };

    // Resolve the program image to launch.
    let Some((name, image)) = crate::user::spawnable_program(frame.a0 as usize) else {
        super::refuse(frame, sc, RefusalReason::UnknownProgram);
        return;
    };

    // Read the caller's `[u32; N]` handle array (bounded).
    const MAX_DELEGATE: usize = 16;
    let n = frame.a2 as usize;
    if n > MAX_DELEGATE {
        super::refuse(frame, sc, RefusalReason::BadUserRange);
        return;
    }
    let mut handles: alloc::vec::Vec<Handle> = alloc::vec::Vec::new();
    if n > 0 {
        let mut buf = [0u8; MAX_DELEGATE * core::mem::size_of::<u32>()];
        let byte_len = n * core::mem::size_of::<u32>();
        let Some(bytes) = crate::user::copy_from_user(frame.a1 as usize, byte_len, &mut buf) else {
            super::refuse(frame, sc, RefusalReason::BadUserRange);
            return;
        };
        handles = bytes
            .chunks_exact(core::mem::size_of::<u32>())
            .map(|c| Handle::from_raw(u32::from_le_bytes([c[0], c[1], c[2], c[3]])))
            .collect();
    }

    // Delegate against the caller's table — all-or-nothing (lock released before
    // we spawn, so the child build never contends on the parent's table).
    let result = {
        let caps = proc.caps.lock();
        kernel_core::cap::delegate(&caps, &handles)
    };
    let Ok(delegated) = result else {
        super::refuse(frame, sc, RefusalReason::CapNotFound);
        return;
    };

    // Build + queue the child on this hart; hand back its task id.
    let child = crate::user::spawn_process_with_caps(
        crate::percpu::current_hartid(),
        name,
        image,
        delegated,
        kernel_core::sched::Priority::Normal,
    );
    frame.a0 = u64::from(child.0);
}
