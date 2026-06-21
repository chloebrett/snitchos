//! Process-lifecycle syscalls: `Exit`, `Wait`, and `Spawn`.

use core::sync::atomic::Ordering;

use crate::trap::TrapFrame;

/// Terminate the calling user process with exit status `a0` (v0.12). Snitches
/// `snitchos.user.exits_total`, clears this hart's current-process pointer,
/// records the exit status + wakes any parent blocked in `Wait` on this task
/// (the reaping bookkeeping), then hands the hart to its next ready task via
/// `sched::exit_now` — which never returns. v0.7b leaks the address space + caps;
/// reclamation is a later milestone.
pub(super) fn handle_exit(frame: &TrapFrame) -> ! {
    let status = frame.a0 as i32;
    let me = crate::sched::current_task_id();

    if let Some(id) = crate::user::user_exits_metric_id() {
        crate::tracing::emit_metric(id, 1);
    }
    crate::process::CURRENT_PROCESS
        .this_cpu()
        .store(core::ptr::null_mut(), Ordering::Relaxed);

    // Record the zombie + wake any parent blocked in `Wait` on us. Must happen
    // before `exit_now` (which never returns). `wake` only re-enqueues a blocked
    // task, so a not-yet-blocked parent (cross-hart racing) is a no-op — fine, as
    // v0.12 `Wait` is same-hart and the parent is already blocked by here.
    if let Some(parent) = crate::sched::note_exit(me, status) {
        crate::sched::wake(parent);
    }

    crate::sched::exit_now()
}

/// Wait for a child to exit and return its status (v0.12). `a0` = the child's
/// task id; returns its exit status in `a0`. Blocks until the child `Exit`s
/// (re-checking on each wake), or returns immediately if it already exited
/// (reaping the zombie). Same-hart in v0.12.
pub(super) fn handle_wait(frame: &mut TrapFrame) {
    use kernel_core::reap::WaitStep;
    use kernel_core::sched::TaskId;

    let me = crate::sched::current_task_id();
    let child = TaskId(frame.a0 as u32);
    loop {
        match crate::sched::wait_for(me, child) {
            WaitStep::Ready(status) => {
                // The child is fully `Exited` and we run in our own address space,
                // so it's safe to reclaim its resources now (frees the child's user
                // AS, `Process`, and kernel stack — see `sched::reap_task`).
                crate::sched::reap_task(child);
                frame.a0 = status as u64;
                return;
            }
            // Recorded as the waiter; block until the child's `Exit` wakes us,
            // then loop to re-check (it'll find the zombie and reap it).
            WaitStep::Block => crate::sched::block_current(),
        }
    }
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
