//! Scheduler smokes and demo task bodies — test scaffolding split out of
//! `sched.rs` so the scheduler core reads as the scheduler core. None of this
//! is production behaviour: the `smoke()` round-trip runs once at boot, and the
//! `*_entry` functions are spawned only under the `workload=` selections that
//! the integration scenarios drive. All of it is asm/`unsafe`/statics-bound, so
//! it stays kernel-side (it can't move to host-buildable `kernel-core`); this
//! module just keeps it out of the way.
//!
//! Re-exported from `sched` (`pub use smoke::…`) so call sites stay
//! `sched::smoke()`, `sched::exit_smoke_entry`, etc.

use alloc::boxed::Box;
use core::sync::atomic::{AtomicU32, Ordering};

use kernel_core::sched::TaskId;

use crate::counter::DeferredCounter;

use super::{
    block_current, current_task_id, exit_now, switch, wake, yield_now, TaskContext, STACK_SIZE,
};

// --- v0.5 step 5 smoke: round-trip the asm without involving the runqueue ---

/// Bumped each time the smoke marker function runs. The heartbeat
/// emits this as `snitchos.sched.smoke_marker_hits`; the integration
/// scenario asserts it's > 0 after boot. `Relaxed`: counter.
pub static SMOKE_MARKER_HITS: DeferredCounter = DeferredCounter::new("snitchos.sched.smoke_marker_hits");

/// Bumped by the `exit_smoke_entry` task body. Heartbeat emits as
/// `snitchos.sched.exit_smoke_hits`; the `sched-task-exits-cleanly`
/// scenario asserts it reaches 1 — proves a spawned task can call
/// `exit_now()` without taking the kernel down. `Relaxed`: counter.
pub static EXIT_SMOKE_HITS: DeferredCounter = DeferredCounter::new("snitchos.sched.exit_smoke_hits");

/// Task body for the exit smoke. Bumps `EXIT_SMOKE_HITS` then
/// terminates via `exit_now`. Spawned once at boot from `kmain`.
pub extern "C" fn exit_smoke_entry() -> ! {
    EXIT_SMOKE_HITS.inc();
    exit_now()
}

/// Bumped once when the `block-wake` smoke's blocker resumes after being
/// woken. Heartbeat emits `snitchos.sched.wake_resumed`; the
/// `block-wake-smoke` scenario asserts it reaches 1. `Relaxed`: counter.
pub static WAKE_RESUMED: DeferredCounter = DeferredCounter::new("snitchos.sched.wake_resumed");

/// Set by the blocker immediately before it blocks. The waker spins yielding
/// until it observes this — so it only ever wakes a task that has already
/// reached `block_current` (and, single-hart non-preemptible, is already
/// `Blocked` by the time the flag is visible: nothing runs between the store
/// and the block's switch). Closes the lost-wakeup window for the smoke.
static BLOCK_WAKE_ARMED: AtomicU32 = AtomicU32::new(0);

/// The blocker's task id, published before it arms; read by the waker.
static BLOCK_WAKE_BLOCKER_ID: AtomicU32 = AtomicU32::new(0);

/// `workload=block-wake` blocker: publish id, arm, block; on resume record
/// the wake and exit.
pub extern "C" fn block_wake_blocker_entry() -> ! {
    BLOCK_WAKE_BLOCKER_ID.store(current_task_id().0, Ordering::Relaxed);
    BLOCK_WAKE_ARMED.store(1, Ordering::Relaxed);
    block_current();
    WAKE_RESUMED.inc();
    exit_now()
}

/// `workload=block-wake` waker: yield until the blocker has armed (so is
/// `Blocked`), wake it, then exit.
pub extern "C" fn block_wake_waker_entry() -> ! {
    while BLOCK_WAKE_ARMED.load(Ordering::Relaxed) == 0 {
        yield_now();
    }
    wake(TaskId(BLOCK_WAKE_BLOCKER_ID.load(Ordering::Relaxed)));
    exit_now()
}

#[repr(C, align(16))]
struct SmokeStack([u8; STACK_SIZE]);

/// Bare `UnsafeCell<TaskContext>` static. Mutex would deadlock since
/// `marker_fn` re-enters the smoke path and tries to lock the same
/// thing. The single-hart, single-thread, one-time nature of the
/// smoke makes raw cell + `&raw mut` correct here.
struct SmokeCtx(core::cell::UnsafeCell<TaskContext>);
// SAFETY: single-hart + single-thread (cooperative v0.5) + the smoke
// runs exactly once during init before any spawn. No concurrent
// access ever, so Sync is vacuous.
unsafe impl Sync for SmokeCtx {}

static SMOKE_MAIN_CTX: SmokeCtx = SmokeCtx(core::cell::UnsafeCell::new(TaskContext {
    ra: 0, sp: 0,
    s0: 0, s1: 0, s2: 0, s3: 0, s4: 0, s5: 0,
    s6: 0, s7: 0, s8: 0, s9: 0, s10: 0, s11: 0,
}));

static SMOKE_MARKER_CTX: SmokeCtx = SmokeCtx(core::cell::UnsafeCell::new(TaskContext {
    ra: 0, sp: 0,
    s0: 0, s1: 0, s2: 0, s3: 0, s4: 0, s5: 0,
    s6: 0, s7: 0, s8: 0, s9: 0, s10: 0, s11: 0,
}));

/// One-time context-switch round-trip smoke. Call once after
/// `heap::init`; never again.
///
/// Builds a marker `TaskContext` with `ra` pointing at
/// `smoke_marker_entry` and `sp` pointing at the top of a freshly
/// allocated 16 KiB stack. Switches into it; the marker bumps
/// `SMOKE_MARKER_HITS` and switches back. After this returns,
/// `SMOKE_MARKER_HITS == 1`.
///
/// If the asm is wrong we either fault (bad ra/sp), return back into
/// main with corrupted state (caller crashes shortly after), or hang
/// (marker never switches back). All three are caught by the
/// integration scenario timing out.
///
/// # Safety
///
/// Call exactly once, after `heap::init`. Single-hart, single-thread
/// context (cooperative v0.5).
pub unsafe fn smoke() {
    // Leak — one-time smoke, the 16 KiB stack belongs to the marker
    // forever. Step 6's `spawn` will track stack ownership properly.
    let stack: Box<SmokeStack> = Box::new(SmokeStack([0u8; STACK_SIZE]));
    let stack_top = (stack.as_ref() as *const _ as u64) + STACK_SIZE as u64;
    core::mem::forget(stack);

    // SAFETY: single-hart, single-thread, smoke runs once at init;
    // no aliasing.
    unsafe {
        let marker = &mut *SMOKE_MARKER_CTX.0.get();
        marker.ra = smoke_marker_entry as *const () as u64;
        marker.sp = stack_top;

        switch(SMOKE_MAIN_CTX.0.get(), SMOKE_MARKER_CTX.0.get());
    }
}

extern "C" fn smoke_marker_entry() -> ! {
    SMOKE_MARKER_HITS.inc();
    // SAFETY: SMOKE_MAIN_CTX was populated by the asm in `smoke()`
    // before this function ran. Switching into it resumes that call
    // site; this function never returns through its own bottom.
    unsafe {
        switch(SMOKE_MARKER_CTX.0.get(), SMOKE_MAIN_CTX.0.get());
    }
    // Unreachable: the switch above transferred control to main's
    // saved ra; this function's frame is now dead. If we ever get
    // here something is profoundly broken — spin so we don't crash
    // silently.
    loop {
        core::hint::spin_loop();
    }
}
