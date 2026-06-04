//! Kernel-side scheduler storage. Owns the task table (`Vec<Box<Task>>`)
//! and the runqueue, both behind a single `kernel::sync::Mutex` so the
//! preempt/IRQ hooks land in one place. The pure-logic
//! `kernel_core::sched::Runqueue` does the actual FIFO bookkeeping;
//! this module wraps it with the kernel-side state (statics, stacks,
//! per-task span cursors) and exposes the `spawn` / `yield_now` /
//! `current_task_id` API the rest of the kernel calls.
//!
//! v0.5 step 4 scope: storage only. `spawn` / `yield_now` /
//! context-switch land in subsequent steps; this step ships the
//! `Task` + `Scheduler` shapes and the static plumbing so the rest of
//! the kernel can already query `current_task_id()` (and emit it on
//! `SpanStart`).
//!
//! See `plans/v0.5-threading.md`.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::arch::global_asm;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use kernel_core::sched::{Runqueue, TaskId, TaskState};
use kernel_core::span::SpanCursor;

use crate::sync::Mutex;

global_asm!(include_str!("sched.S"));

/// Saved callee-saved register set for a kernel thread that's
/// off-CPU. Layout matches `sched.S` byte-for-byte — do not reorder
/// or add fields without updating the asm offsets.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct TaskContext {
    pub ra: u64,
    pub sp: u64,
    pub s0: u64,
    pub s1: u64,
    pub s2: u64,
    pub s3: u64,
    pub s4: u64,
    pub s5: u64,
    pub s6: u64,
    pub s7: u64,
    pub s8: u64,
    pub s9: u64,
    pub s10: u64,
    pub s11: u64,
}

unsafe extern "C" {
    /// Save callee-saved regs into `from`, load them from `to`,
    /// return. To both threads' code, this looks like a normal
    /// function call.
    ///
    /// # Safety
    ///
    /// - Both pointers must be valid for the duration of the call
    ///   and point at exclusive `TaskContext` storage.
    /// - `to` must either be a `TaskContext` previously populated by
    ///   a successful `switch(_, to)` (i.e. the thread was paused via
    ///   `switch` and is now being resumed), or a freshly-rigged
    ///   context whose `ra` points at a `extern "C" fn() -> !`
    ///   and whose `sp` is the top of an exclusive, sufficiently
    ///   aligned stack region.
    pub fn switch(from: *mut TaskContext, to: *mut TaskContext);
}

/// One kernel thread. Today: id, name, scheduler state, span cursor,
/// stats. Steps 5+ add the saved register context and stack
/// ownership; step 2's `span::SpanCursor` already gives each task its
/// own innermost-span tracker.
pub struct Task {
    pub id: TaskId,
    pub name: String,
    pub state: TaskState,
    pub span_cursor: SpanCursor,
    /// Total time on-CPU in `time`-CSR ticks. Bumped on every yield
    /// out of this task; read by the heartbeat to emit
    /// `snitchos.task.cpu_time_ticks{task=name}`.
    pub cpu_time_ticks: AtomicU64,
    /// How many times the scheduler has picked this task.
    pub runs: AtomicU64,
}

impl Task {
    fn new(id: TaskId, name: String, state: TaskState) -> Self {
        Self {
            id,
            name,
            state,
            span_cursor: SpanCursor::new(),
            cpu_time_ticks: AtomicU64::new(0),
            runs: AtomicU64::new(0),
        }
    }
}

/// Global scheduler state. Owned by `static SCHEDULER`. The task list
/// is a `Vec<Box<Task>>` so individual `Task` allocations don't move
/// when the vector grows — context-switch will hand the asm a stable
/// raw pointer per task.
pub struct Scheduler {
    /// All known tasks, indexed by their position in this vec. `id.0`
    /// equals `tasks[i].id.0`; the vec is never reordered.
    tasks: Vec<Box<Task>>,
    runqueue: Runqueue,
}

impl Scheduler {
    const fn new() -> Self {
        Self {
            tasks: Vec::new(),
            runqueue: Runqueue::new(),
        }
    }

    pub fn task_count(&self) -> usize {
        self.tasks.len()
    }

    pub fn runqueue_depth(&self) -> usize {
        self.runqueue.len()
    }

    /// Iterate the task table for telemetry purposes (heartbeat
    /// emits per-task metrics by walking this).
    pub fn tasks(&self) -> &[Box<Task>] {
        &self.tasks
    }
}

/// The kernel's single scheduler. Const-init so it lands in `.bss`
/// rather than requiring a `Once`. Future preempt/IRQ-disable hooks
/// inside `Mutex::lock` cover every access uniformly.
pub static SCHEDULER: Mutex<Scheduler> = Mutex::new(Scheduler::new());

/// Allocator for new task ids. Monotonically increasing; never
/// recycles. `Task 0` is reserved for the boot context, allocated
/// when `init_with_current_as_main` runs in step 8.
static NEXT_TASK_ID: AtomicU32 = AtomicU32::new(0);

fn alloc_task_id() -> TaskId {
    TaskId(NEXT_TASK_ID.fetch_add(1, Ordering::Relaxed))
}

/// Currently-running task. v0.5 step 4 stub: returns 0 (the boot /
/// main task) unconditionally. Step 7 wires this to the real
/// scheduler bookkeeping; until then `SpanStart` carries `task_id=0`
/// uniformly, which is correct because there genuinely is only one
/// running task.
pub fn current_task_id() -> TaskId {
    TaskId(CURRENT_TASK.load(Ordering::Relaxed))
}

/// Storage for "which task is on CPU right now," per-CPU eventually
/// (wrapped in `PerCpu<AtomicU32>` once SMP arrives). For v0.5
/// single-hart it's a plain atomic; v0.5.x preempt + v0.7+ SMP can
/// swap this to `PerCpu<AtomicU32>` without touching callers.
static CURRENT_TASK: AtomicU32 = AtomicU32::new(0);

/// Install a freshly-built task into the table without a stack or
/// context. v0.5 step 4 scope: lets the boot path register itself as
/// task 0 so `current_task_id()` and SpanStart task_id round-trip
/// correctly. Spawning real threads (with stacks + entry functions)
/// lands in step 6.
pub fn register_bare_task(name: &str, state: TaskState) -> TaskId {
    let id = alloc_task_id();
    let task = Box::new(Task::new(id, String::from(name), state));
    SCHEDULER.lock().tasks.push(task);
    id
}

// --- v0.5 step 5 smoke: round-trip the asm without involving the runqueue ---

/// Bumped each time the smoke marker function runs. The heartbeat
/// emits this as `snitchos.sched.smoke_marker_hits`; the integration
/// scenario asserts it's > 0 after boot.
pub static SMOKE_MARKER_HITS: AtomicU64 = AtomicU64::new(0);

#[repr(C, align(16))]
struct SmokeStack([u8; 16384]);

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
    let stack: Box<SmokeStack> = Box::new(SmokeStack([0u8; 16384]));
    let stack_top = (stack.as_ref() as *const _ as u64) + 16384;
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
    SMOKE_MARKER_HITS.fetch_add(1, Ordering::Relaxed);
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
