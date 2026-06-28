//! Kernel-side telemetry: timestamp source, string interning, span ID
//! allocation, and the `span!` macro that emits `Frame::SpanStart` /
//! `Frame::SpanEnd` around a block of code.
//!
//! All frames go out the virtio-console (`virtio_console::send`). The
//! host-reader decodes them.

use protocol::{Frame, MetricKind, SpanId, StringId};

use kernel_core::clock::Clock;
use kernel_core::intern::InternTable;
use kernel_core::preinit::PreInitBuffer;
use kernel_core::sink::FrameSink;
use kernel_core::span::{self, SpanCursor, SpanIds};

use crate::trap::CLOCK;
use crate::virtio_console;

/// Read the RISC-V `time` CSR — a monotonically increasing 64-bit cycle
/// counter clocked at the rate the DTB reports as `timebase-frequency`.
///
/// Convert to seconds on the host side via `ticks / timebase_hz`; the
/// kernel never does the division (avoids overflow in the
/// `ticks * 10^9 / hz` form, and keeps the math out of the hot path).
pub fn timestamp() -> u64 {
    CLOCK.now()
}

/// Encode a `Frame::Hello` with the given CPU timebase and ship it out
/// the virtio-console. The very first frame on the wire — tells the host
/// what `timebase_hz` to use when interpreting subsequent timestamps.
pub fn send_hello(timebase_hz: u32) {
    let frame = Frame::Hello {
        timebase_hz: timebase_hz as u64,
        protocol_version: protocol::PROTOCOL_VERSION,
    };
    let mut buf = [0u8; 32];
    if let Ok(encoded) = postcard::to_slice(&frame, &mut buf) {
        virtio_console::send(encoded);
    }
}

// --- String intern table ---
//
// The table logic lives in `kernel_core::intern::InternTable`. The
// kernel binary holds the one global instance behind a Mutex, plus a
// `KernelSink` adapter that routes frame emits through `emit_frame`.

static INTERN_TABLE: crate::sync::Mutex<InternTable> = crate::sync::Mutex::new(InternTable::new());

/// Running total of interned names reclaimed on process exit — bumped by
/// [`release_names`], drained by the heartbeat as
/// `snitchos.intern.strings_released_total`. The deferred-emission pattern (bump
/// an atomic in the path, emit from the heartbeat) keeps `release_names` off the
/// telemetry TX lock it would otherwise re-enter. Pairs with the live
/// `strings_used` gauge: used drops, released climbs.
static STRINGS_RELEASED: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// The kernel's single `FrameSink`: encode the frame with postcard,
/// then either ship to the virtio-console (if it's up) or append to
/// the pre-init buffer for later flush.
///
/// The 512-byte encode buffer holds span / event / metric frames, short
/// `StringRegister`s, and a `Frame::Log` line (a userspace `println!`, capped
/// at `MAX_USER_STR_LEN` bytes of message + framing). Frames that don't fit are
/// silently dropped — encoding failure is a programmer error (frame too big or
/// postcard bug), distinct from pre-init overflow which the buffer counts and
/// the host learns about via `Frame::Dropped`.
struct KernelSink;

impl FrameSink for KernelSink {
    fn emit(&mut self, frame: &Frame<'_>) {
        let mut buf = [0u8; 512];
        let Ok(bytes) = postcard::to_slice(frame, &mut buf) else {
            return;
        };
        if virtio_console::CONSOLE.get().is_some() {
            virtio_console::send(bytes);
        } else {
            PRE_INIT_BUFFER.lock().append(bytes);
        }
    }
}

/// Look up `name` in the intern table. If it's new, assign a fresh
/// `StringId` and emit a `Frame::StringRegister` so the host learns
/// the mapping. If it's already known, return the existing id without
/// emitting.
///
/// Known weakness: holds the table lock during the wire emit. Locking
/// order is intern → virtio_console::CONSOLE. As long as nothing else
/// takes them in the opposite order we're fine; v0.1 has no other lockers.
pub fn register_or_lookup(name: &'static str) -> StringId {
    INTERN_TABLE.lock().register_or_lookup(name, &mut KernelSink)
}

/// Snitch a **refused syscall** — a first-class observability event so a
/// denied U-mode request is never silent (the whole point: a refusal you can
/// see beats a result frame that never appeared). `syscall` is the raw `a7`,
/// `reason` says what failed; task and hart are stamped here. Safe from a
/// synchronous syscall handler (same emit path as `emit_metric`).
pub fn emit_syscall_refused(syscall: u8, reason: protocol::RefusalReason) {
    emit_frame(&Frame::SyscallRefused {
        syscall,
        reason,
        task_id: crate::sched::current_task_id().0,
        t: timestamp(),
        hart_id: crate::percpu::current_hartid() as u8,
    });
}

/// Emit a userspace **stdout line** (from the `DebugWrite` syscall) as a `Log`
/// frame, attributed to the current task. Stdout-as-telemetry: a `println!`
/// becomes an observable wire event the collector can surface.
pub fn emit_log(msg: &str) {
    emit_frame(&Frame::Log {
        msg,
        task_id: crate::sched::current_task_id().0,
        t: timestamp(),
        hart_id: crate::percpu::current_hartid() as u8,
    });
}

/// Intern `name` (a runtime string copied from U-mode) and open a span on the
/// calling task's cursor, returning the `{id, parent}` close token (which
/// userspace holds, just as the kernel's `Span` guard does). Non-RAII twin of
/// [`span_start`]: the matching `SpanEnd` comes from a later
/// [`span_close_checked`].
///
/// Names are scoped **per process** via `span_names` (the caller's own
/// [`SpanNameTable`]): a name the process has used before resolves to the
/// `StringId` it already registered (no re-register); a genuinely new name
/// registers a fresh id — including a name the kernel or another process also
/// uses, so the process gets its *own* distinct id and cannot emit under another's
/// span name nor probe the global name set. A new name past the table's quota is
/// **refused** (returns `None`) without allocating. The interned name is owned
/// (not leaked) and reclaimed on process exit. The resolve, quota check, and
/// registration all run under the per-process span-name lock, so the decision is
/// precise.
///
/// [`SpanNameTable`]: kernel_core::span_name::SpanNameTable
pub fn span_open_bounded(
    name: &str,
    span_names: &crate::sync::Mutex<kernel_core::span_name::SpanNameTable>,
) -> Option<span::SpanOpen> {
    let name_id = {
        let mut names = span_names.lock();
        if let Some(id) = names.resolve(name) {
            id
        } else if names.is_full() {
            return None;
        } else {
            // Owned, not leaked: the intern table and this process's table each
            // hold a copy under the same id, both reclaimed when the process is
            // reaped (`release_names` walks `SpanNameTable::ids`). Pre-GC this
            // `Box::leak`'d a fresh `&'static` per spawn — the bound this fixes.
            let id = INTERN_TABLE
                .lock()
                .register_owned(alloc::boxed::Box::<str>::from(name), &mut KernelSink);
            names.insert(alloc::boxed::Box::<str>::from(name), id);
            id
        }
    };
    let cursor = current_cursor();
    let open = span::open(&SPAN_IDS, cursor);
    emit_frame(&Frame::SpanStart {
        id: open.id,
        parent: open.parent,
        name_id,
        t: timestamp(),
        task_id: crate::sched::current_task_id().0,
        hart_id: crate::percpu::current_hartid() as u8,
    });
    Some(open)
}

/// Close a userspace span: the caller hands back the `{id, parent}` token from
/// [`span_open_owned`]. Validates `id` against the **cursor top** — only the
/// innermost open span may close — refusing an out-of-order or forged id
/// (returns `false`). On success emits `SpanEnd` and restores the cursor to
/// `parent`. The cursor is per-task, so a bad close can only desync the
/// caller's own cursor.
#[must_use]
pub fn span_close_checked(id: SpanId, parent: SpanId) -> bool {
    let cursor = current_cursor();
    if cursor.current() != id {
        return false;
    }
    emit_frame(&Frame::SpanEnd { id, t: timestamp() });
    span::close(cursor, &span::SpanOpen { id, parent });
    true
}

/// Number of names currently registered in the intern table. Exposed
/// as a metric (`snitchos.intern.strings_used`).
pub fn intern_count() -> u32 {
    INTERN_TABLE.lock().count()
}

/// Reclaim a set of interned names by id — called from `reap_task` with the
/// exiting process's span + metric ids (`SpanNameTable::ids` + `MetricTable::ids`).
/// Each [`StringId`] is tombstoned in the intern table: its bytes are dropped and
/// the id is never reused (wire-identity stability). Ids the process doesn't own
/// (or already-released ones) are harmless no-ops.
pub fn release_names(ids: impl IntoIterator<Item = StringId>) {
    let mut table = INTERN_TABLE.lock();
    let freed = ids.into_iter().filter(|&id| table.release(id)).count();
    drop(table);
    STRINGS_RELEASED.fetch_add(freed as u64, core::sync::atomic::Ordering::Relaxed);
}

/// Total interned names reclaimed on process exit so far. Exposed as the
/// `snitchos.intern.strings_released_total` counter.
pub fn strings_released_total() -> u64 {
    STRINGS_RELEASED.load(core::sync::atomic::Ordering::Relaxed)
}

/// Register `name` as a Counter metric. Returns its `StringId` for use
/// with `emit_metric`. Idempotent — safe to call every iteration of a
/// loop; the host only sees one `MetricRegister`.
pub fn register_counter(name: &'static str) -> StringId {
    INTERN_TABLE
        .lock()
        .register_metric(name, MetricKind::Counter, protocol::NO_EMITTER, &mut KernelSink)
}

/// Like `register_counter` but takes an owned `String` — the kernel
/// leaks it into `'static`. Use only for runtime-built names whose
/// total count is bounded (e.g. per-task metric names registered
/// once per task at spawn time). Every call commits ~bytes_of(name)
/// to the heap forever.
pub fn register_counter_owned(name: alloc::string::String) -> StringId {
    let leaked: &'static str = alloc::boxed::Box::leak(name.into_boxed_str());
    register_counter(leaked)
}

/// Register `name` as a Gauge metric.
pub fn register_gauge(name: &'static str) -> StringId {
    INTERN_TABLE
        .lock()
        .register_metric(name, MetricKind::Gauge, protocol::NO_EMITTER, &mut KernelSink)
}

/// Like `register_gauge` but takes an owned `String` — the kernel leaks it into
/// `'static`. Use only for runtime-built names whose total count is bounded
/// (e.g. one per task at spawn). The gauge twin of `register_counter_owned`.
pub fn register_gauge_owned(name: alloc::string::String) -> StringId {
    let leaked: &'static str = alloc::boxed::Box::leak(name.into_boxed_str());
    register_gauge(leaked)
}

/// Register `name` as a Histogram metric.
pub fn register_histogram(name: &'static str) -> StringId {
    INTERN_TABLE
        .lock()
        .register_metric(name, MetricKind::Histogram, protocol::NO_EMITTER, &mut KernelSink)
}

/// Register a **userspace-named** metric of `kind` from a runtime-copied name,
/// returning its `StringId`. The intern table *owns* `name` (reclaimed when the
/// process is reaped — see [`release_names`]), and every call allocates a *fresh*
/// id — there is **no** content dedup, by design: each process's metric is its
/// own `StringId`, so one process can't forge another's (or the kernel's own)
/// telemetry. Bounded by the caller's per-process `MetricTable` quota, checked
/// *before* this runs. Backs the `RegisterMetric` syscall. The registering task is
/// stamped on the `MetricRegister` (the emitter dimension), so the collector keeps
/// two processes that name a metric identically as distinct Prometheus series.
pub fn register_user_metric(name: &str, kind: MetricKind) -> StringId {
    let task_id = crate::sched::current_task_id().0;
    INTERN_TABLE.lock().register_metric_owned(
        alloc::boxed::Box::<str>::from(name),
        kind,
        task_id,
        &mut KernelSink,
    )
}

/// Emit a metric sample. The name must have been registered first via
/// `register_counter` / `register_gauge` / `register_histogram`.
pub fn emit_metric(name_id: StringId, value: i64) {
    emit_frame(&Frame::Metric {
        name_id,
        value,
        t: timestamp(),
        hart_id: crate::percpu::current_hartid() as u8,
    });
}

/// Emit a `ThreadRegister` frame. Called once per `sched::spawn` so
/// the collector can resolve `task_id` → human-readable name on
/// subsequent `SpanStart` frames and OTLP `thread.name` attributes.
/// `priority` is the task's static scheduling level (`Priority as u8`:
/// 0 = Low, 1 = Normal, 2 = High) so the trace can group/colour by it.
pub fn emit_thread_register(
    id: kernel_core::sched::TaskId,
    name: &str,
    priority: kernel_core::sched::Priority,
) {
    emit_frame(&Frame::ThreadRegister { id: id.0, name, priority: priority as u8 });
}

/// Emit a `HartRegister` frame. Called once per hart at bring-up so
/// the collector can resolve `hart_id` → role (and platform
/// `mhartid` for correlation with hardware docs).
pub fn emit_hart_register(id: u8, mhartid: u64, role: protocol::HartRole) {
    emit_frame(&Frame::HartRegister { id, mhartid, role });
}

/// Emit a `CapEvent::Granted` frame — the kernel snitching authority being
/// *created*. Richer than the `cap.grants_total` counter (that is a rate,
/// this an attributed fact): carries the global `cap_id`, the `holder`
/// identity, the object kind, and the granted `rights`, so the host can
/// reconstruct the capability derivation tree. `parent_cap_id` is `0`
/// (root) — no derivation until v0.8.
pub fn emit_cap_granted(cap_id: u64, holder: u32, object: protocol::CapObject, rights: u32) {
    emit_frame(&Frame::CapEvent {
        kind: protocol::CapEventKind::Granted,
        cap_id,
        parent_cap_id: 0,
        holder,
        object,
        rights,
        // Bootstrap grants carry no badge; badged minting emits its own (Step 5).
        badge: 0,
        t: timestamp(),
        hart_id: crate::percpu::current_hartid() as u8,
    });
}

/// Emit a `CapEvent::Transferred` frame — a capability handed from one holder
/// to another. `parent_cap_id` names the **source holding** the transferred cap
/// derived from (the derivation edge), or `0` where no precise parent is tracked
/// yet (e.g. the kernel minting a one-shot reply cap at a `call` rendezvous —
/// linking that to the originating `call` is a later refinement).
pub fn emit_cap_transferred(
    cap_id: u64,
    parent_cap_id: u64,
    holder: u32,
    object: protocol::CapObject,
    rights: u32,
    badge: u64,
) {
    emit_frame(&Frame::CapEvent {
        kind: protocol::CapEventKind::Transferred,
        cap_id,
        parent_cap_id,
        holder,
        object,
        rights,
        badge,
        t: timestamp(),
        hart_id: crate::percpu::current_hartid() as u8,
    });
}

/// Emit a `NotifySignal` frame (v0.12): `from_task` signalled `notification`
/// with `mask`. The producer half of the async kernel→user edge — paired with
/// [`emit_notify_wait`] it makes the out-of-band wake visible in a trace.
pub fn emit_notify_signal(notification: u32, mask: u64, from_task: u32) {
    emit_frame(&Frame::NotifySignal {
        notification,
        mask,
        from_task,
        t: timestamp(),
        hart_id: crate::percpu::current_hartid() as u8,
    });
}

/// Emit a `NotifyWait` frame (v0.12): `to_task` woke on `notification` carrying
/// `bits` (read-and-cleared). The consumer half of the async edge.
pub fn emit_notify_wait(notification: u32, bits: u64, to_task: u32) {
    emit_frame(&Frame::NotifyWait {
        notification,
        bits,
        to_task,
        t: timestamp(),
        hart_id: crate::percpu::current_hartid() as u8,
    });
}

/// Emit a `ContextSwitch` frame. Called by `sched::yield_now` on
/// every actual switch. Makes scheduler decisions first-class
/// traceable events.
pub fn emit_context_switch(
    from: kernel_core::sched::TaskId,
    to: kernel_core::sched::TaskId,
    reason: protocol::SwitchReason,
) {
    emit_frame(&Frame::ContextSwitch {
        from: from.0,
        to: to.0,
        t: timestamp(),
        reason,
        hart_id: crate::percpu::current_hartid() as u8,
    });
}

/// Emit a `Message` frame for an IPC rendezvous: a message crossed from task
/// `from` to task `to` over `endpoint`, carrying `parent` (the sender's span)
/// as the boundary-crossing trace link. Called by the receive path at delivery
/// — outside the endpoint critical section, and the frame is string-free (no
/// intern, no alloc), so direct emission is safe (same context as
/// `emit_metric` in the `Invoke` handler).
pub fn emit_message(endpoint: u32, from: u32, to: u32, parent: protocol::SpanId) {
    emit_frame(&Frame::Message {
        endpoint,
        from,
        to,
        parent_span: parent,
        t: timestamp(),
        hart_id: crate::percpu::current_hartid() as u8,
    });
}

/// Open a span named `$name` for the current scope. Expands to a
/// `let _span = ...` binding so the guard lives until the caller's
/// scope ends. The span's `SpanEnd` frame fires automatically when
/// the guard drops.
///
/// ```ignore
/// fn boot() {
///     span!("kernel.boot");
///     {
///         span!("serial_init");
///         // ... whatever the span covers ...
///     }  // serial_init ends here
/// }  // kernel.boot ends here
/// ```
///
/// Implementation note: this MUST be a statement-emitting macro, not a
/// block-expression. If it were `{ let _g = ...; }` the guard would
/// drop at the end of the macro's block, ending the span immediately
/// instead of at the caller's scope boundary.
#[macro_export]
macro_rules! span {
    ($name:expr) => {
        let _span = $crate::tracing::span_start($name);
    };
}

// --- Span machinery ---
//
// Two pieces:
//
//   - `SPAN_IDS`: global monotonic id allocator. One static, shared
//     across all tasks. Ids stay unique across the system.
//   - Per-task `SpanCursor`: each `Task` owns one (via `Task.span_cursor`).
//     `sched::CURRENT_SPAN_CURSOR` points at the running task's cursor;
//     yield_now updates it on every context switch.
//   - `FALLBACK_CURSOR`: used for spans opened before task 0 is registered
//     (the pre-init kernel.boot region). After register_bare_task seeds
//     CURRENT_SPAN_CURSOR, new spans use per-task cursors.
//
// Each `Span` guard remembers the cursor it was opened on, so close
// always pops from the right stack — even if the running task has
// changed in between.
static SPAN_IDS: SpanIds = SpanIds::new();
static FALLBACK_CURSOR: SpanCursor = SpanCursor::new();

fn current_cursor() -> &'static SpanCursor {
    let ptr = crate::sched::CURRENT_SPAN_CURSOR.this_cpu().load(core::sync::atomic::Ordering::Relaxed);
    if ptr.is_null() {
        &FALLBACK_CURSOR
    } else {
        // SAFETY: ptr points at a `SpanCursor` inside a `Box<Task>` in
        // `SCHEDULER.tasks`. Tasks are never reaped in v0.5, so the
        // Task allocation lives forever and the cursor address stays
        // valid.
        unsafe { &*ptr }
    }
}

/// The innermost open span on the running task's cursor, or `SpanId(0)` if
/// none. The IPC `send` path reads this to carry the sender's trace context to
/// the receiver (so the receiver's handling span becomes its child).
pub fn current_span_id() -> protocol::SpanId {
    current_cursor().current()
}

/// Seed the running task's cursor with an incoming parent span, so its next
/// [`span_start`] (or U-mode `SpanOpen`) opens a child of `parent`. The IPC
/// `receive` path calls this with the sender's span id — the kernel-populated
/// trace context crossing the process boundary. `SpanId(0)` is a no-op-shaped
/// seed (the next span is a root), so an IPC with no open sender span is safe.
pub fn set_current_parent(parent: protocol::SpanId) {
    current_cursor().set_current(parent);
}

/// RAII guard returned by `span_start`. Drop emits `SpanEnd` and
/// pops the span off the cursor it was opened on.
///
/// `cursor` is the cursor that was current at `span_start` time. We
/// keep it so a span that survives a context switch closes on the
/// same cursor it opened on, rather than picking up whichever task
/// happens to be running at Drop time.
///
/// Known weakness: `mem::forget(span)` skips Drop, leaking the span
/// (no SpanEnd on the wire) and leaving the cursor pointing at this
/// span forever. The `span!` macro avoids handing the user a name-bound
/// guard they could forget.
pub struct Span {
    open: kernel_core::span::SpanOpen,
    cursor: *const SpanCursor,
}

impl Drop for Span {
    fn drop(&mut self) {
        emit_frame(&Frame::SpanEnd {
            id: self.open.id,
            t: timestamp(),
        });
        // SAFETY: cursor was valid at span_start; pointer to a
        // `SpanCursor` inside a `Box<Task>` (or the static fallback)
        // stays valid for the lifetime of this kernel.
        let cursor = unsafe { &*self.cursor };
        span::close(cursor, &self.open);
    }
}

/// Open a span named `name`. Returns a `Span` guard whose `Drop` will
/// emit `SpanEnd`. Nesting is automatic from Rust scopes.
pub fn span_start(name: &'static str) -> Span {
    let name_id = register_or_lookup(name);
    let cursor = current_cursor();
    let open = span::open(&SPAN_IDS, cursor);
    emit_frame(&Frame::SpanStart {
        id: open.id,
        parent: open.parent,
        name_id,
        t: timestamp(),
        task_id: crate::sched::current_task_id().0,
        hart_id: crate::percpu::current_hartid() as u8,
    });
    Span { open, cursor: cursor as *const _ }
}

// --- Frame emission, with pre-init buffering ---

/// Bytes we buffer up before `virtio_console::init` has completed. The
/// storage and append/drain mechanics live in `kernel_core::preinit`;
/// this is just the kernel's one instance.
static PRE_INIT_BUFFER: crate::sync::Mutex<PreInitBuffer> = crate::sync::Mutex::new(PreInitBuffer::new());

/// Flush any frames buffered while the virtio-console was still
/// initializing. Call this exactly once, right after
/// `virtio_console::init` succeeds.
///
/// Always follows with a `Frame::Dropped { count }` — the host treats
/// this as a positive "buffer flushed, here's the loss count"
/// checkpoint. `count == 0` means no frames were lost.
pub fn flush_pre_init() {
    let dropped = PRE_INIT_BUFFER
        .lock()
        .drain(virtio_console::send);
    emit_frame(&Frame::Dropped { count: dropped });
}

/// Thin wrapper for module-internal call sites that want to emit a
/// frame without naming `KernelSink`. Equivalent to constructing one
/// and calling `.emit`.
fn emit_frame(frame: &Frame<'_>) {
    KernelSink.emit(frame);
}
