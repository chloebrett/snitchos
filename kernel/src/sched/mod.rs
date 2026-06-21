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
use core::sync::atomic::{AtomicPtr, AtomicU32, AtomicU64, AtomicUsize, Ordering};

use protocol::SwitchReason;

use kernel_core::sched::{
    address_space_switch, on_cpu_delta, pick_next, quantum_expired, should_preempt, Candidate,
    CurrentDisposition, Priority, Runqueue, TaskId, TaskState,
};
use kernel_core::span::SpanCursor;

use crate::counter::DeferredCounter;
use crate::process::Process;

use crate::percpu::{PerCpu, MAX_HARTS};
use crate::sync::Mutex;

global_asm!(include_str!("sched.S"));

/// Scheduler smokes + demo task bodies (test scaffolding). Re-exported below so
/// call sites stay `sched::smoke()`, `sched::exit_smoke_entry`, etc.
mod smoke;
pub use smoke::{
    block_wake_blocker_entry, block_wake_waker_entry, exit_smoke_entry, smoke, EXIT_SMOKE_HITS,
    SMOKE_MARKER_HITS, WAKE_RESUMED,
};

/// Userspace process / address-space bookkeeping (`process`) and the demo task
/// bodies (`demo_tasks`). Re-exported at the crate root so call sites stay
/// `crate::process`, `crate::demo_tasks`.
pub mod demo_tasks;
pub mod process;

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

    /// Load-only sibling of `switch`. Loads callee-saved + sp + ra
    /// from `to` and `ret`s into the resumed thread. Used by
    /// `exit_now` to abandon the current task without saving its
    /// register state.
    ///
    /// # Safety
    ///
    /// `to` must point at a valid populated `TaskContext` whose `ra`
    /// is a callable address and whose `sp` is the top of a valid
    /// stack region exclusively held by that task. After this runs,
    /// the caller's stack and registers are forgotten — the calling
    /// task is gone.
    pub fn switch_into(to: *mut TaskContext) -> !;
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
    /// Set to `Exited` by [`exit_now`]; consumers (`task_count`,
    /// `task_snapshots`) filter exited entries so the heartbeat
    /// gauges don't keep reporting them after the task is gone.
    /// `Ready` / `Running` distinctions aren't currently load-bearing
    /// outside the runqueue, but the value is correct.
    pub state: TaskState,
    pub span_cursor: SpanCursor,
    /// The user address space this task runs in: the root page-table PA,
    /// or `0` for a kernel task (`main`/`idle`), which runs under whatever
    /// root is loaded — the kernel high-half is mapped into every space.
    /// Set by `user::run` (via [`set_current_address_space`]) once the task
    /// has built its `Process`, before it `enter`s U-mode. The scheduler
    /// reads it on switch to decide whether to reload `satp`.
    /// `Relaxed`: written once by the task itself, read by the same hart's
    /// scheduler; no cross-hart publication of pointed-at state here.
    pub address_space: AtomicUsize,
    /// Pointer to this task's [`Process`] (for `CURRENT_PROCESS` on switch),
    /// or null for a kernel task. Set alongside [`Task::address_space`]; the
    /// `Process` lives in `user::run`'s never-returning frame, so the pointer
    /// stays valid. `Relaxed`: same single-writer-same-hart-reader discipline
    /// as `CURRENT_PROCESS` itself.
    pub process: AtomicPtr<Process>,
    /// Static scheduling priority (v0.8b). Set at spawn (default `Normal`),
    /// immutable thereafter, read under the scheduler lock when the task is
    /// (re-)enqueued to build its [`Candidate`]. A plain field (not atomic)
    /// because it's written once on the owned `Task` before it enters the table.
    /// (The task's *wait clock* lives on the runqueue `Candidate`, not here — it
    /// changes per enqueue, so it belongs with the queue entry.)
    pub priority: Priority,
    /// A generic per-task argument word, set at spawn and read by the task once
    /// it runs (via [`current_task_arg`]). The scheduler treats it as opaque —
    /// it carries no meaning here. Userspace program launch uses it as a program
    /// selector (the spec address); `0` for tasks spawned without an arg. Plain
    /// field: written once on the owned `Task` before it enters the table, then
    /// read by the task itself.
    pub arg: usize,
    /// Total time on-CPU in `time`-CSR ticks. Bumped on every yield
    /// out of this task; read by the heartbeat to emit
    /// `snitchos.task.<name>.cpu_time_ticks`. `Relaxed`: counter.
    pub cpu_time_ticks: AtomicU64,
    /// How many times the scheduler has picked this task.
    /// `Relaxed`: counter.
    pub runs: AtomicU64,
    /// Pre-registered metric ids so the heartbeat emit path doesn't
    /// re-format strings per tick. Populated by `spawn` /
    /// `register_bare_task`. Sentinel (`StringId(0)`) under the
    /// `workload=spawn-storm` selection where the per-task emit loop is
    /// skipped.
    pub cpu_time_metric: protocol::StringId,
    pub runs_metric: protocol::StringId,
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
        // The spawn storm spawns ~200 tasks back-to-back. Each
        // `register_counter_owned` call mints a fresh leaked 'static str
        // whose pointer becomes a new intern-table entry (the
        // `register_or_lookup` path is pointer-keyed, so each fresh leak
        // is distinct — `lookup_by_content` would dedupe but isn't used
        // here), so 200 spawns × 2 metrics would permanently leak ~400
        // 'static strings and grow the intern table for a throwaway
        // stress workload. The
        // heartbeat's per-task metric emit loop is also skipped under that
        // workload, so a sentinel StringId is fine. (`boot_workload::selected()`
        // is set in `kmain` before any task is created.)
        let (cpu_time_metric, runs_metric) = if crate::boot_workload::selected()
            == Some(kernel_core::bootargs::WorkloadKind::SpawnStorm)
        {
            (protocol::StringId(0), protocol::StringId(0))
        } else {
            (
                crate::tracing::register_counter_owned(alloc::format!(
                    "snitchos.task.{name}.cpu_time_ticks"
                )),
                crate::tracing::register_counter_owned(alloc::format!(
                    "snitchos.task.{name}.runs_total"
                )),
            )
        };
        Self {
            id,
            name,
            state,
            span_cursor: SpanCursor::new(),
            address_space: AtomicUsize::new(0),
            process: AtomicPtr::new(core::ptr::null_mut()),
            priority: Priority::Normal,
            arg: 0,
            cpu_time_ticks: AtomicU64::new(0),
            runs: AtomicU64::new(0),
            cpu_time_metric,
            runs_metric,
            context: UnsafeCell::new(TaskContext::default()),
            _stack: None,
        }
    }
}

/// Global scheduler state. Owned by `static SCHEDULER`. The task list
/// is a `Vec<Box<Task>>` so individual `Task` allocations don't move
/// when the vector grows — context-switch will hand the asm a stable
/// raw pointer per task.
///
/// v0.6 step 10: `runqueues` is per-hart. Each hart pops from its own
/// runqueue in `yield_now`, so cross-hart spawns (`spawn_on`) land in
/// the target hart's queue. There is no work-stealing — an idle hart
/// with an empty runqueue runs its idle task and `wfi`s until an IPI
/// arrives saying "you have new work."
pub struct Scheduler {
    /// All known tasks, indexed by their position in this vec. `id.0`
    /// equals `tasks[i].id.0`; the vec is never reordered.
    #[allow(
        clippy::vec_box,
        reason = "the Box is load-bearing: it gives each Task a stable heap address so the raw `*mut TaskContext` / `*const SpanCursor` pointers stay valid across Vec growth and past the scheduler-mutex drop"
    )]
    tasks: Vec<Box<Task>>,
    /// One runqueue per hart. Hart `i` pops from `runqueues[i]`.
    runqueues: [Runqueue; crate::percpu::MAX_HARTS],
}

impl Scheduler {
    const fn new() -> Self {
        Self {
            tasks: Vec::new(),
            runqueues: [const { Runqueue::new() }; crate::percpu::MAX_HARTS],
        }
    }

    /// Number of *live* tasks in the table. Exited tasks remain in
    /// `tasks` (leaked, no reaping yet) but are filtered here so the
    /// heartbeat `snitchos.sched.tasks_total` gauge tracks the
    /// scheduler's actual workload rather than its lifetime spawn
    /// count.
    pub fn task_count(&self) -> usize {
        self.tasks
            .iter()
            .filter(|t| t.state != TaskState::Exited)
            .count()
    }

    /// Depth of `hartid`'s runqueue. Lock-protected access from the
    /// heartbeat (which reads the boot hart's depth as the scheduler
    /// gauge today; per-hart depth gauges land if needed).
    pub fn runqueue_depth(&self, hartid: usize) -> usize {
        self.runqueues[hartid].len()
    }

    /// Iterate the task table for telemetry purposes (heartbeat
    /// emits per-task metrics by walking this).
    #[expect(
        dead_code,
        reason = "task-table accessor; heartbeat currently drains via task_snapshots(), this is kept for direct iteration"
    )]
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
/// the runqueue was empty don't count). `Relaxed`: pure counter.
pub static CONTEXT_SWITCHES: DeferredCounter = DeferredCounter::new("snitchos.sched.context_switches_total");

/// Cumulative count of **preemptions** — context switches the timer forced
/// because a userspace task overran its quantum (`reschedule(Preempt)` that
/// actually switched). A subset of `CONTEXT_SWITCHES`. Bumped in the reschedule
/// path (an atomic, never a frame from the timer handler) and drained by the
/// heartbeat as `snitchos.sched.preemptions_total`. `Relaxed`: pure counter.
pub static PREEMPTIONS: DeferredCounter = DeferredCounter::new("snitchos.sched.preemptions_total");

/// Time spent in `yield_now`'s bookkeeping (everything from function
/// entry up to but not including the `switch` asm). Captures the
/// scheduler's per-yield overhead — lock acquisition, runqueue
/// manipulation, context-pointer lookup, the `ContextSwitch` frame
/// emission. Does NOT include the asm itself (a handful of cycles)
/// or the time off-CPU (which is "everyone else's time," not ours).
/// Sampled by the heartbeat into a histogram. `Relaxed`: last-value
/// snapshot, no payload.
pub static LAST_YIELD_OVERHEAD_TICKS: AtomicU64 = AtomicU64::new(0);

/// Allocator for new task ids. Monotonically increasing; never
/// recycles. `Task 0` is reserved for the boot context, allocated
/// when `init_with_current_as_main` runs in step 8.
/// `Relaxed`: the atomic *is* the id allocation; no payload.
static NEXT_TASK_ID: AtomicU32 = AtomicU32::new(0);

fn alloc_task_id() -> TaskId {
    TaskId(NEXT_TASK_ID.fetch_add(1, Ordering::Relaxed))
}

/// Snapshot of scheduler counts. Cheap; briefly takes the scheduler
/// lock. Called by the heartbeat each tick.
pub struct SchedStats {
    pub tasks_total: usize,
    pub runqueue_depth: usize,
}

pub fn stats() -> SchedStats {
    let sched = SCHEDULER.lock();
    // Per-hart runqueue: report the calling hart's depth. The boot
    // hart's heartbeat is what reads this today; multi-hart per-runqueue
    // depth gauges would require a different metric shape.
    let me = crate::percpu::current_hartid();
    SchedStats {
        tasks_total: sched.task_count(),
        runqueue_depth: sched.runqueue_depth(me),
    }
}

/// Per-task snapshot for metric emission. Briefly takes the scheduler
/// lock to walk the task table, allocates an owned name string per
/// task so the caller can drop the lock before doing slow virtio
/// emits.
pub struct TaskSnapshot {
    pub cpu_time_metric: protocol::StringId,
    pub runs_metric: protocol::StringId,
    pub cpu_time_ticks: u64,
    pub runs: u64,
}

pub fn task_snapshots() -> Vec<TaskSnapshot> {
    let sched = SCHEDULER.lock();
    sched
        .tasks
        .iter()
        .filter(|t| t.state != TaskState::Exited)
        .map(|t| TaskSnapshot {
            cpu_time_metric: t.cpu_time_metric,
            runs_metric: t.runs_metric,
            cpu_time_ticks: t.cpu_time_ticks.load(Ordering::Relaxed),
            runs: t.runs.load(Ordering::Relaxed),
        })
        .collect()
}

/// Currently-running task. v0.5 step 4 stub: returns 0 (the boot /
/// main task) unconditionally. Step 7 wires this to the real
/// scheduler bookkeeping; until then `SpanStart` carries `task_id=0`
/// uniformly, which is correct because there genuinely is only one
/// running task.
pub fn current_task_id() -> TaskId {
    TaskId(CURRENT_TASK.this_cpu().load(Ordering::Relaxed))
}

/// The generic `arg` word of the task running on this hart, or `0` if it was
/// spawned without one. Set by [`spawn_on_with_arg`] and read once the task
/// starts — userspace `program_entry` uses it as a program selector (a
/// `ProgramSpec` address) so one generic entry serves every program.
pub fn current_task_arg() -> usize {
    let id = current_task_id();
    let sched = SCHEDULER.lock();
    sched.tasks.iter().find(|t| t.id == id).map_or(0, |t| t.arg)
}

/// Whether this hart's runqueue holds any `Ready` task (the caller, running, is
/// off-queue). Idle loops check this before `wfi`: a hart must never sleep
/// while it has runnable work, or a just-woken task (e.g. an IPC receiver the
/// rendezvous made `Ready`) is stranded until the next timer IRQ breaks `wfi`.
pub fn has_ready_tasks() -> bool {
    let me = crate::percpu::current_hartid();
    SCHEDULER.lock().runqueue_depth(me) > 0
}

/// Associate the currently-running task with the user address space it is
/// about to enter: its root page-table PA and its [`Process`]. Called by
/// `user::run` after building the process but before `enter`, so that when
/// the scheduler later switches *back* into this task it reloads `satp` and
/// republishes `CURRENT_PROCESS`. Without this, a second userspace process
/// would resume under the previous process's address space.
///
/// `proc` must point at a `Process` that outlives every future run of this
/// task — `user::run`'s frame never returns, satisfying that.
pub fn set_current_address_space(root_pa: usize, proc: *mut Process) {
    let current_id = TaskId(CURRENT_TASK.this_cpu().load(Ordering::Relaxed));
    let sched = SCHEDULER.lock();
    for task in &sched.tasks {
        if task.id == current_id {
            task.address_space.store(root_pa, Ordering::Relaxed);
            task.process.store(proc, Ordering::Relaxed);
            break;
        }
    }
}

/// The user address-space root (`root_pa`) of the task named by `id`, or
/// `None` if no such task or it has no user space (a kernel task, `root_pa`
/// 0). The cross-AS copy primitive uses this to reach a blocked caller's page
/// table: the caller is named by the reply cap, resolved to its `TaskId`, then
/// to the `root_pa` walked here.
pub fn address_space_of(id: TaskId) -> Option<usize> {
    let sched = SCHEDULER.lock();
    sched
        .tasks
        .iter()
        .find(|task| task.id == id)
        .map(|task| task.address_space.load(Ordering::Relaxed))
        .filter(|&root_pa| root_pa != 0)
}

/// Storage for "which task is on CPU right now," lifted to `PerCpu`
/// in v0.6 step 5. Single-hart through step 10: every access reads /
/// writes `[0]`. Under multi-hart, each hart sees its own slot and
/// the call sites stay identical.
///
/// `Relaxed` everywhere: per-CPU means only the owning hart touches
/// this slot, so there is no cross-hart observer to order against.
/// See `kernel::percpu` for the kernel-wide ordering discipline.
static CURRENT_TASK: PerCpu<AtomicU32> =
    PerCpu::new([const { AtomicU32::new(0) }; MAX_HARTS]);

/// Timestamp when the current task last became `Running`. On every
/// `yield_now` we compute `now - CURRENT_TASK_ENTRY_TICK` and add to
/// the outgoing task's `cpu_time_ticks` — this is the per-task
/// on-CPU time accumulator.
///
/// Initial value 0 is "uninitialised"; we lazy-init on the first
/// `yield_now` rather than during boot so we don't have to thread a
/// timestamp through `register_bare_task`. `Relaxed`: per-CPU.
static CURRENT_TASK_ENTRY_TICK: PerCpu<AtomicU64> =
    PerCpu::new([const { AtomicU64::new(0) }; MAX_HARTS]);

/// Pointer to the current task's `SpanCursor`. Updated on context
/// switch and during `register_bare_task` for task 0. Read by
/// `tracing::span_start` so each task's span stack is independent —
/// task A's open spans don't end up as task B's parents.
///
/// Initial value null: any span opened before task 0 is registered
/// (the pre-init kernel.boot region) falls back to the static
/// cursor in `tracing::SPAN_CURSOR`. Span guards remember which
/// cursor opened them so close happens on the right one, even if
/// the current pointer has moved on.
///
/// `Relaxed`: per-CPU pointer; the pointed-at `SpanCursor` lives
/// inside `Box<Task>` (stable heap address) and has its own atomic
/// for the open-span stack. The pointer publication doesn't need
/// to publish the `SpanCursor`'s contents because the next reader
/// is on the same hart (yield lands in the next task on this CPU).
pub static CURRENT_SPAN_CURSOR: PerCpu<AtomicPtr<SpanCursor>> =
    PerCpu::new([const { AtomicPtr::new(core::ptr::null_mut()) }; MAX_HARTS]);

/// Install a freshly-built task into the table without a stack or
/// context. v0.5 step 4 scope: lets the boot path register itself as
/// task 0 so `current_task_id()` and SpanStart task_id round-trip
/// correctly. Spawning real threads (with stacks + entry functions)
/// lands in step 6.
pub fn register_bare_task(name: &str, state: TaskState) -> TaskId {
    let id = alloc_task_id();
    let task = Box::new(Task::new_bare(id, String::from(name), state));
    let owned_name = task.name.clone();
    // Pointer to this task's cursor — stable because Box<Task> heap
    // address won't change. Used to seed `CURRENT_SPAN_CURSOR` so the
    // calling hart's span emissions parent correctly under this task.
    let cursor_ptr = (&task.span_cursor as *const SpanCursor) as *mut SpanCursor;
    SCHEDULER.lock().tasks.push(task);
    // Seed this hart's per-CPU current-task slots so subsequent
    // `current_task_id()`, span emissions, and the first `yield_now`
    // see *this* task, not the default (task 0). Pre-step-10 this
    // worked by coincidence because hart 0 always registered first
    // and got id=0 (matching the AtomicU32 default); hart 1 gets a
    // non-zero id and would silently impersonate task 0 without this.
    CURRENT_TASK.this_cpu().store(id.0, Ordering::Relaxed);
    if CURRENT_SPAN_CURSOR.this_cpu().load(Ordering::Relaxed).is_null() {
        CURRENT_SPAN_CURSOR.this_cpu().store(cursor_ptr, Ordering::Relaxed);
    }
    crate::tracing::emit_thread_register(id, &owned_name, Priority::Normal);
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
    spawn_on(crate::percpu::current_hartid(), name, entry)
}

/// Spawn a new kernel thread on a specific hart. The task lands on
/// `hart`'s runqueue; if `hart != current_hartid()`, an `IPI_WAKEUP`
/// is sent to that hart so it breaks out of `wfi` and notices the
/// new work. v0.6 step 10 caller: kmain spawning workload tasks
/// could leave them on hart 0 (`spawn`) or migrate consumer to
/// hart 1 (`spawn_on(1, ...)`); the latter is step 11's headline.
pub fn spawn_on(hart: usize, name: &str, entry: extern "C" fn() -> !) -> TaskId {
    spawn_on_with_priority(hart, name, entry, Priority::Normal)
}

/// Like [`spawn_on`] but at an explicit scheduling priority (v0.8b). Higher
/// priority runs preferentially; aging keeps lower ones from starving. The
/// default-priority `spawn`/`spawn_on` keep every existing call site at
/// `Normal`, so behaviour is unchanged until a workload spawns at distinct
/// levels.
pub fn spawn_on_with_priority(
    hart: usize,
    name: &str,
    entry: extern "C" fn() -> !,
    priority: Priority,
) -> TaskId {
    spawn_on_with_arg(hart, name, entry, 0, priority)
}

/// Like [`spawn_on_with_priority`] but also stashes a generic `arg` word on the
/// task, readable once it runs via [`current_task_arg`]. The scheduler carries
/// `arg` opaquely; userspace launch uses it as a program selector so one generic
/// entry can serve every program. `arg = 0` is the no-argument default that the
/// other `spawn*` helpers pass.
pub fn spawn_on_with_arg(
    hart: usize,
    name: &str,
    entry: extern "C" fn() -> !,
    arg: usize,
    priority: Priority,
) -> TaskId {
    debug_assert!(hart < crate::percpu::MAX_HARTS);
    let id = alloc_task_id();
    let stack: Box<Stack> = Box::new(Stack::new_zeroed());
    let sp = stack.top_addr();

    let mut task = Box::new(Task::new_bare(id, String::from(name), TaskState::Ready));
    task.priority = priority;
    task.arg = arg;
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
        // Enter the ready set now; stamp the wait clock (= now) for aging.
        sched.runqueues[hart].push_back(Candidate {
            id,
            base: priority,
            enqueued_tick: crate::tracing::timestamp(),
        });
    }
    // Under the spawn storm we skip ThreadRegister so the spawn path
    // has no MMIO (virtio) write between writing the new `ctx.ra/sp`
    // and sending the IPI. The whole point of the storm is to maximise
    // per-trial race exposure; an MMIO write here acquires the BQL and
    // would silently close the race window each iteration. See
    // `plans/residual-race-investigation.md` appendix A.
    if crate::boot_workload::selected()
        != Some(kernel_core::bootargs::WorkloadKind::SpawnStorm)
    {
        crate::tracing::emit_thread_register(id, &owned_name, priority);
    } else {
        let _ = owned_name;
    }

    // Cross-hart spawn: wake the target so it picks up the new task
    // instead of staying in wfi indefinitely.
    if hart != crate::percpu::current_hartid() {
        crate::ipi::send(hart, crate::ipi::IPI_WAKEUP);
    }
    id
}

/// Reload `satp` and `CURRENT_PROCESS` if the task being switched into
/// lives in a different user address space than the one currently loaded.
///
/// `next_root` is the incoming task's root PA (`0` for a kernel task);
/// `next_proc` its `Process` pointer. A kernel task (`root == 0`) runs under
/// whatever space is loaded — the high-half is shared — so nothing changes.
/// A user task whose root already matches `satp` (e.g. an `idle` ran under it
/// in between) needs no reload either. Only a genuine cross-address-space
/// switch writes the CSR (`mmu::activate` does the `sfence.vma`) and
/// republishes the process for the trap handler. Must run *after* the
/// scheduler lock drops and *before* the `switch` asm, so the resumed task
/// `sret`s under its own address space.
fn switch_address_space(next_root: usize, next_proc: *mut Process) {
    let next = (next_root != 0).then_some(next_root);
    if let Some(root) = address_space_switch(crate::mmu::current_satp_root(), next) {
        crate::mmu::activate(root);
        crate::process::CURRENT_PROCESS
            .this_cpu()
            .store(next_proc, Ordering::Relaxed);
    }
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
    reschedule(SwitchReason::Yield);
}

/// The shared core of every context switch. Under the scheduler lock: pick the
/// highest effective-priority ready task on this hart, dispose of the current
/// task per `disposition`, load the next task's context/cursor/address-space +
/// accounting, set the per-hart current-task state, and emit the
/// `ContextSwitch` tagged `reason`. Returns the `(current, next)` context
/// pointers for the caller's `switch` (two-way) or `switch_into` (one-way).
///
/// Returns `None` when the runqueue is empty: if `empty_ok` the caller should
/// just keep running (`reschedule`/`block_current`); otherwise — `exit_now`,
/// which has nowhere to go — it panics instead.
///
/// The pick is a single O(n) scan of this hart's ready queue (each `Candidate`
/// carries its own priority + wait clock — base priority boosted by how long it
/// waited (aging), ties broken by longest wait → FIFO-fair). The running task
/// is off the queue, so it can't be re-picked. With every task at the default
/// `Normal`, this reduces to FIFO.
fn prepare_switch(
    disposition: CurrentDisposition,
    reason: SwitchReason,
    empty_ok: bool,
) -> Option<(*mut TaskContext, *mut TaskContext)> {
    let t_entry = crate::tracing::timestamp();
    let current_id = TaskId(CURRENT_TASK.this_cpu().load(Ordering::Relaxed));
    let me = crate::percpu::current_hartid();

    let (current_ctx, next_ctx, next_id, next_root, next_proc) = {
        let mut sched = SCHEDULER.lock();
        let Some(next_id) = pick_next(sched.runqueues[me].iter(), t_entry, AGING_STEP_TICKS) else {
            assert!(empty_ok, "prepare_switch: runqueue empty on hart {me} with no fallback");
            return None; // nothing else ready — caller keeps running
        };
        sched.runqueues[me].remove(next_id);

        // Single pass through the task table: dispose of the outgoing task,
        // accumulate its on-CPU time, and capture both context pointers + the
        // incoming task's address space. `Box<Task>` keeps each `Task` at a
        // stable heap address even if the `Vec` reallocates, so the raw
        // pointers stay valid past the lock drop.
        let prev_entry = CURRENT_TASK_ENTRY_TICK.this_cpu().load(Ordering::Relaxed);
        let cpu_delta = on_cpu_delta(prev_entry, t_entry);
        let mut current_ctx: *mut TaskContext = core::ptr::null_mut();
        let mut current_priority = Priority::Normal;
        let mut next_ctx: *mut TaskContext = core::ptr::null_mut();
        let mut next_cursor: *mut SpanCursor = core::ptr::null_mut();
        let mut next_root: usize = 0;
        let mut next_proc: *mut Process = core::ptr::null_mut();
        for task in sched.tasks.iter_mut() {
            if task.id == current_id {
                current_ctx = task.context.get();
                current_priority = task.priority;
                task.cpu_time_ticks.fetch_add(cpu_delta, Ordering::Relaxed);
                if let Some(state) = disposition.next_state() {
                    task.state = state;
                }
            }
            if task.id == next_id {
                next_ctx = task.context.get();
                next_cursor = core::ptr::from_ref(&task.span_cursor).cast_mut();
                next_root = task.address_space.load(Ordering::Relaxed);
                next_proc = task.process.load(Ordering::Relaxed);
                task.runs.fetch_add(1, Ordering::Relaxed);
            }
        }
        assert!(!current_ctx.is_null(), "prepare_switch: current task missing from table");
        assert!(!next_ctx.is_null(), "prepare_switch: next task missing from table");

        if disposition.requeues() {
            // The outgoing task re-enters the ready set now — stamp its wait
            // clock (= now) so aging measures from this moment.
            sched.runqueues[me].push_back(Candidate {
                id: current_id,
                base: current_priority,
                enqueued_tick: t_entry,
            });
        }
        CURRENT_TASK.this_cpu().store(next_id.0, Ordering::Relaxed);
        CURRENT_SPAN_CURSOR.this_cpu().store(next_cursor, Ordering::Relaxed);

        (current_ctx, next_ctx, next_id, next_root, next_proc)
        // Lock dropped here. The asm runs without the scheduler lock.
    };

    switch_address_space(next_root, next_proc);

    CONTEXT_SWITCHES.inc();
    if matches!(reason, SwitchReason::Preempt) {
        PREEMPTIONS.inc();
    }
    crate::tracing::emit_context_switch(current_id, next_id, reason);

    let t_before_switch = crate::tracing::timestamp();
    LAST_YIELD_OVERHEAD_TICKS.store(t_before_switch.wrapping_sub(t_entry), Ordering::Relaxed);
    // The next task is about to become Running. Record its entry tick now
    // (close enough — the switch asm is a handful of cycles); it computes its
    // on-CPU delta from this on its next switch out.
    CURRENT_TASK_ENTRY_TICK.this_cpu().store(t_before_switch, Ordering::Relaxed);

    Some((current_ctx, next_ctx))
}

/// Core context switch: pick the next ready task on this hart and switch into
/// it, re-enqueuing the current task. `yield_now` passes `Yield` (voluntary);
/// v0.8's timer preemption passes `Preempt` (involuntary). Returns immediately
/// if the runqueue is empty. Preemption is *layered* on this same cooperative
/// switch — the preempted task's full state lives in its `TrapFrame` on its
/// kernel stack; this switch only swaps the 14 callee-saved regs + sp.
fn reschedule(reason: SwitchReason) {
    if let Some((current_ctx, next_ctx)) =
        prepare_switch(CurrentDisposition::Requeue, reason, true)
    {
        // SAFETY: both pointers are into `UnsafeCell<TaskContext>` storage in
        // stable `Box<Task>` allocations owned by `SCHEDULER.tasks`. The asm
        // has exclusive access for the duration of the call (cooperative
        // single-hart; no preemption mid-switch). We resume here on a later
        // switch back into us.
        unsafe { switch(current_ctx, next_ctx) };
    }
}

/// Time slice a userspace task may run before the timer preempts it, in
/// `time`-CSR ticks. The QEMU timer fires every ~1 s (timebase), which bounds
/// the *effective* granularity to a tick; this quantum (well under that) means
/// any user task still on-CPU at a timer tick has overrun and is descheduled,
/// while a cooperative task (sub-millisecond slices) never accumulates a full
/// quantum and is never forcibly preempted. Per-priority quanta are a v0.8b
/// follow-on.
pub const QUANTUM_TICKS: u64 = 2_000_000; // 0.2 s at 10 MHz

/// Aging step for priority scheduling (v0.8b): a ready task's effective
/// priority rises one level per this many ticks it waits, so a starved low
/// task eventually out-bids steady higher-priority work. 1 s at 10 MHz — a
/// `Low` task reaches `High` after ~2 s of waiting, visible but bounded.
/// Cooperative tasks re-enqueue far faster than this, so they never age.
pub const AGING_STEP_TICKS: u64 = 10_000_000;

/// Timer-driven preemption entry point (v0.8). Called from the timer IRQ
/// handler with whether the interrupted code was in **user** mode (`SPP == 0`).
///
/// The kernel is never preempted: if `from_user` is false we return at once,
/// keeping the cooperative "exclusive until I yield" guarantee for kernel code.
/// If a userspace task has used up its [`QUANTUM_TICKS`], reschedule with reason
/// `Preempt` — the layered switch parks its full `TrapFrame` on its own kernel
/// stack and runs the next ready task; `reschedule` returns immediately if
/// nothing else is ready.
///
/// Emitting the `ContextSwitch{Preempt}` inline (inside `reschedule`) is safe
/// *because* of the `from_user` gate: the interrupted context was in U-mode, so
/// it held no kernel `Mutex` on this hart — the telemetry TX path it runs
/// through can't re-entrant-deadlock against the thing we interrupted.
pub fn maybe_preempt(from_user: bool) {
    if !from_user {
        return;
    }
    let now = crate::tracing::timestamp();
    let entry = CURRENT_TASK_ENTRY_TICK.this_cpu().load(Ordering::Relaxed);
    if !quantum_expired(entry, now, QUANTUM_TICKS) {
        return;
    }
    // Priority-aware (v0.8b): only preempt if a ready task is of equal-or-higher
    // *effective* priority than the running one. The timer time-slices within a
    // level and yields to a higher arrival, but never demotes a higher-priority
    // task to a lower one (that would be priority inversion). Aging lets a
    // long-starved low task reach the running level and so preempt even a hog.
    let current_id = TaskId(CURRENT_TASK.this_cpu().load(Ordering::Relaxed));
    let me = crate::percpu::current_hartid();
    let due = {
        let sched = SCHEDULER.lock();
        let current_level = sched
            .tasks
            .iter()
            .find(|t| t.id == current_id)
            .map_or(Priority::Normal as u8, |t| t.priority as u8);
        should_preempt(current_level, sched.runqueues[me].iter(), now, AGING_STEP_TICKS)
    };
    if due {
        reschedule(SwitchReason::Preempt);
    }
}

/// Terminate the calling task. Marks it `Exited`, picks the next
/// ready task on this hart, and switches into it via the load-only
/// `switch_into` asm. Never returns.
///
/// The exited task's `Box<Task>` and `Box<Stack>` remain allocated
/// in `SCHEDULER.tasks` — v0.5.x minimal-scope variant; reaping
/// lands later. `task_count` and `task_snapshots` filter out
/// `Exited` entries so the heartbeat doesn't keep reporting them.
///
/// Open spans on the calling task's cursor are NOT auto-closed —
/// the caller must balance them first. The cursor itself becomes
/// inert (nothing reads it after exit).
///
/// # Panics
///
/// If the runqueue is empty when this is called — there's nothing
/// to switch into. Storm scenarios ensure `hart_1_main` stays on
/// hart 1's queue specifically to keep this invariant.
pub fn exit_now() -> ! {
    // `empty_ok = false`: a terminating task has nowhere to fall back to, so an
    // empty runqueue is fatal (prepare_switch panics rather than returning).
    // Re-use `Yield` on the wire — a dedicated `Exit` reason is deferred until a
    // host consumer needs to distinguish it.
    let (_current_ctx, next_ctx) = prepare_switch(CurrentDisposition::Exit, SwitchReason::Yield, false)
        .expect("prepare_switch with empty_ok=false returns Some or panics");

    // SAFETY: `next_ctx` points at the `UnsafeCell<TaskContext>` of a live
    // `Box<Task>` in `SCHEDULER.tasks`. The exiting task's stack is abandoned,
    // but its `Box<Task>` is leaked (not freed), so no dangling reference. The
    // load-only `switch_into` `ret`s into the next task on its own stack — the
    // calling task's `sp` is gone the instant it writes the new one, so we
    // never save the exiting context. Never returns.
    unsafe { switch_into(next_ctx) }
}

/// Block the calling task: mark it `Blocked`, switch to the next ready task
/// **without** re-enqueuing the caller, and park until a later [`wake`]
/// returns it to the runqueue and the scheduler picks it — at which point
/// control returns past the `switch` here. The third sibling of
/// [`yield_now`]/[`reschedule`] (re-enqueues, two-way `switch`) and
/// [`exit_now`] (no re-enqueue, one-way `switch_into`): block does *not*
/// re-enqueue (like exit) but *does* save its context for resumption (like
/// yield), so it uses the two-way `switch`.
///
/// v0.9 IPC's blocking `send`/`receive` are this function's callers. The
/// endpoint `Mutex` MUST be dropped before calling this (lock discipline:
/// never hold a `kernel::sync::Mutex` across a switch).
///
/// Idle is always ready, so a target always exists; if the runqueue is
/// somehow empty we keep running rather than deadlock the hart (a blocked
/// caller with nothing to switch to is a caller bug).
pub fn block_current() {
    // `empty_ok = true`: idle is normally ready so a target exists, but if the
    // runqueue is somehow empty we keep running rather than strand the hart (a
    // blocked caller with nothing to switch to is a caller bug, not a panic).
    if let Some((current_ctx, next_ctx)) =
        prepare_switch(CurrentDisposition::Block, SwitchReason::Blocked, true)
    {
        // SAFETY: both pointers are into stable `Box<Task>` contexts owned by
        // `SCHEDULER.tasks`. The two-way `switch` saves our callee-saved regs +
        // sp into `current_ctx`, so when a later `wake` re-enqueues us and the
        // scheduler picks us, control returns right here. Single-hart
        // cooperative; no preemption mid-switch.
        unsafe { switch(current_ctx, next_ctx) };
    }
    // Resumed (a `wake` picked us), or nothing was ready and we kept running.
}

/// Return a `Blocked` task to the runqueue so the scheduler can pick it.
/// The [`kernel_core::sched::on_wake`] guard makes this idempotent: waking a
/// task that is already `Ready`/`Running`/`Exited` (a racing or duplicate
/// wake) is a no-op, so a task is never double-enqueued. Enqueues on the
/// current hart's runqueue (single-hart v0.9 core; cross-hart wake — enqueue
/// on the task's home hart + IPI — is the deferred follow-on).
pub fn wake(id: TaskId) {
    let now = crate::tracing::timestamp();
    let me = crate::percpu::current_hartid();
    let mut sched = SCHEDULER.lock();
    let priority = sched.tasks.iter_mut().find(|t| t.id == id).and_then(|task| {
        if kernel_core::sched::on_wake(task.state) {
            task.state = TaskState::Ready;
            Some(task.priority)
        } else {
            None
        }
    });
    if let Some(base) = priority {
        sched.runqueues[me].push_back(Candidate { id, base, enqueued_tick: now });
    }
}
