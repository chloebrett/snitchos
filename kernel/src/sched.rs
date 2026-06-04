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
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use kernel_core::sched::{Runqueue, TaskId, TaskState};
use kernel_core::span::SpanCursor;

use crate::sync::Mutex;

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
