//! Process-lifecycle syscall: `Exit`. v0.7b has no spawn/teardown surface yet;
//! this is the only lifecycle op (the room this module leaves to grow).

use core::sync::atomic::Ordering;

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
