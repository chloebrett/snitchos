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
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use protocol::{Frame, SwitchReason};

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

/// Per-task stack size in bytes. 16 KiB is generous for kernel work
/// (the call graphs we have today don't get deep); cheap on 128 MiB.
pub const STACK_SIZE: usize = 16384;

/// Stack with 16-byte alignment so RISC-V's `extern "C"` ABI is
/// satisfied on first entry. Used both by `spawn()`-built tasks and
/// by the v0.5-step-5 smoke.
#[repr(C, align(16))]
pub struct Stack([u8; STACK_SIZE]);

impl Stack {
    pub const fn new_zeroed() -> Self {
        Self([0u8; STACK_SIZE])
    }
    pub fn top_addr(&self) -> u64 {
        (self as *const _ as u64) + STACK_SIZE as u64
    }
}

/// One kernel thread. The `context` field holds the saved
/// callee-saved register set while the task is off-CPU; the asm
/// reads/writes it through a `*mut TaskContext`. `_stack` keeps the
/// stack memory alive — the raw `sp` value in `context` points into
/// it, so the `Box` must outlive any running of this task.
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
    /// Saved register state while off-CPU. `UnsafeCell` because the
    /// asm needs `*mut` access while the `Task` is borrowed `&` from
    /// the scheduler's `Vec`. The mutex around the scheduler
    /// serialises any access to `Task`; the asm holds exclusive
    /// access through the `*mut` for the duration of the switch.
    pub context: UnsafeCell<TaskContext>,
    /// Backing storage for the task's stack. `None` for task 0
    /// which inherits the boot stack. Field is read by no one
    /// directly; it's here for `Drop` to free the stack when the
    /// task is reaped.
    _stack: Option<Box<Stack>>,
}

// SAFETY: Task contains an UnsafeCell<TaskContext> (which is !Sync).
// Access is serialised through the SCHEDULER mutex; the asm holds an
// exclusive `*mut` for the duration of a `switch` and there is no
// concurrent reader on the single-hart cooperative v0.5.
unsafe impl Sync for Task {}

impl Task {
    fn new_bare(id: TaskId, name: String, state: TaskState) -> Self {
        Self {
            id,
            name,
            state,
            span_cursor: SpanCursor::new(),
            cpu_time_ticks: AtomicU64::new(0),
            runs: AtomicU64::new(0),
            context: UnsafeCell::new(TaskContext::default()),
            _stack: None,
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

/// Cumulative count of context switches the scheduler has performed.
/// Bumped per `yield_now` that actually switched (no-op yields when
/// the runqueue was empty don't count).
pub static CONTEXT_SWITCHES: AtomicU64 = AtomicU64::new(0);

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
    let task = Box::new(Task::new_bare(id, String::from(name), state));
    let owned_name = task.name.clone();
    SCHEDULER.lock().tasks.push(task);
    crate::tracing::emit_thread_register(id, &owned_name);
    id
}

/// Spawn a new kernel thread. Allocates a 16 KiB stack, rigs a
/// `TaskContext` so the first `switch` into the task lands in
/// `entry`, registers the task with the scheduler, and pushes it
/// onto the runqueue. Emits a `ThreadRegister` frame so the
/// collector can resolve the task id to a name.
///
/// The task does not run immediately — it sits on the runqueue
/// until the scheduler picks it (step 7's `yield_now`).
///
/// `entry` is `extern "C" fn() -> !` so the function's call ABI
/// matches what the asm hands it on first switch, and the type
/// system says it won't return (because if it does, we don't have
/// anywhere to return *to*).
pub fn spawn(name: &str, entry: extern "C" fn() -> !) -> TaskId {
    let id = alloc_task_id();
    let stack: Box<Stack> = Box::new(Stack::new_zeroed());
    let sp = stack.top_addr();

    let mut task = Box::new(Task::new_bare(id, String::from(name), TaskState::Ready));
    // SAFETY: we have unique ownership of `task`; nothing else
    // references it yet.
    unsafe {
        let ctx = &mut *task.context.get();
        ctx.ra = entry as *const () as u64;
        ctx.sp = sp;
    }
    task._stack = Some(stack);

    let owned_name = task.name.clone();
    {
        let mut sched = SCHEDULER.lock();
        sched.tasks.push(task);
        sched.runqueue.push_back(id);
    }
    crate::tracing::emit_thread_register(id, &owned_name);
    id
}

/// Voluntarily yield CPU to the next task on the runqueue. The
/// current task is pushed onto the back of the runqueue; the next
/// task is popped from the front and switched into. If the runqueue
/// is empty, returns immediately (nothing else wants the CPU).
///
/// Cooperative-v0.5: every kernel thread is expected to call this
/// periodically (or at any blocking point). Preempt-disable for
/// not-yet-existent preemption + lock-discipline ("don't yield while
/// holding a `kernel::sync::Mutex`") is on the caller for now.
pub fn yield_now() {
    let current_id = TaskId(CURRENT_TASK.load(Ordering::Relaxed));

    let (current_ctx, next_ctx, next_id) = {
        let mut sched = SCHEDULER.lock();
        let Some(next_id) = sched.runqueue.pop_front() else {
            return; // nothing else ready — keep running
        };
        if next_id == current_id {
            // Shouldn't happen — current task is supposed to be off the
            // runqueue while running — but defensively don't switch
            // into ourselves (would corrupt the saved context).
            sched.runqueue.push_back(next_id);
            return;
        }

        // Single pass through the task table to capture both context
        // pointers + bump the picked task's runs counter. Box<Task>
        // means the Task itself sits at a stable heap address even if
        // the Vec reallocates, so the raw pointers stay valid past
        // the lock drop.
        let mut current_ctx: *mut TaskContext = core::ptr::null_mut();
        let mut next_ctx: *mut TaskContext = core::ptr::null_mut();
        for task in &sched.tasks {
            if task.id == current_id {
                current_ctx = task.context.get();
            }
            if task.id == next_id {
                next_ctx = task.context.get();
                task.runs.fetch_add(1, Ordering::Relaxed);
            }
        }
        assert!(!current_ctx.is_null(), "current task missing from table");
        assert!(!next_ctx.is_null(), "next task missing from table");

        sched.runqueue.push_back(current_id);
        CURRENT_TASK.store(next_id.0, Ordering::Relaxed);

        (current_ctx, next_ctx, next_id)
        // Lock dropped here. The asm runs without the scheduler lock.
    };

    CONTEXT_SWITCHES.fetch_add(1, Ordering::Relaxed);
    crate::tracing::emit_context_switch(current_id, next_id, SwitchReason::Yield);

    // SAFETY: both pointers point at `UnsafeCell<TaskContext>` storage
    // inside `Box<Task>` allocations owned by `SCHEDULER.tasks`. The
    // `Vec` may reallocate its slice of `Box` pointers, but the
    // `Task` allocations sit at stable heap addresses. The asm has
    // exclusive access to both for the duration of the call (cooperative
    // single-hart; no preemption mid-switch).
    unsafe { switch(current_ctx, next_ctx) };
    // We've been resumed (a future yield switched back into us).
}

// --- v0.5 step 5 smoke: round-trip the asm without involving the runqueue ---

/// Bumped each time the smoke marker function runs. The heartbeat
/// emits this as `snitchos.sched.smoke_marker_hits`; the integration
/// scenario asserts it's > 0 after boot.
pub static SMOKE_MARKER_HITS: AtomicU64 = AtomicU64::new(0);

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
