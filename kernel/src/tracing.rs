//! Kernel-side telemetry: timestamp source, string interning, span ID
//! allocation, and the `span!` macro that emits `Frame::SpanStart` /
//! `Frame::SpanEnd` around a block of code.
//!
//! All frames go out the virtio-console (`virtio_console::send`). The
//! host-reader decodes them.

use protocol::{Frame, MetricKind, StringId};

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
        protocol_version: 1,
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

/// The kernel's single `FrameSink`: encode the frame with postcard,
/// then either ship to the virtio-console (if it's up) or append to
/// the pre-init buffer for later flush.
///
/// The 128-byte encode buffer is sized for span / event / metric
/// frames and short `StringRegister`s (the longest name we register is
/// ~30 chars). Frames that don't fit are silently dropped — encoding
/// failure is a programmer error (frame too big or postcard bug),
/// distinct from pre-init overflow which the buffer counts and the
/// host learns about via `Frame::Dropped`.
struct KernelSink;

impl FrameSink for KernelSink {
    fn emit(&mut self, frame: &Frame<'_>) {
        let mut buf = [0u8; 128];
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

/// Number of names currently registered in the intern table. Exposed
/// as a metric (`snitchos.intern.strings_used`).
pub fn intern_count() -> u32 {
    INTERN_TABLE.lock().count()
}

/// Register `name` as a Counter metric. Returns its `StringId` for use
/// with `emit_metric`. Idempotent — safe to call every iteration of a
/// loop; the host only sees one `MetricRegister`.
pub fn register_counter(name: &'static str) -> StringId {
    INTERN_TABLE
        .lock()
        .register_metric(name, MetricKind::Counter, &mut KernelSink)
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
        .register_metric(name, MetricKind::Gauge, &mut KernelSink)
}

/// Register `name` as a Histogram metric.
pub fn register_histogram(name: &'static str) -> StringId {
    INTERN_TABLE
        .lock()
        .register_metric(name, MetricKind::Histogram, &mut KernelSink)
}

/// Emit a metric sample. The name must have been registered first via
/// `register_counter` / `register_gauge` / `register_histogram`.
pub fn emit_metric(name_id: StringId, value: i64) {
    emit_frame(&Frame::Metric {
        name_id,
        value,
        t: timestamp(),
    });
}

/// Emit a `ThreadRegister` frame. Called once per `sched::spawn` so
/// the collector can resolve `task_id` → human-readable name on
/// subsequent `SpanStart` frames and OTLP `thread.name` attributes.
pub fn emit_thread_register(id: kernel_core::sched::TaskId, name: &str) {
    emit_frame(&Frame::ThreadRegister { id: id.0, name });
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
    let ptr = crate::sched::CURRENT_SPAN_CURSOR.load(core::sync::atomic::Ordering::Relaxed);
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
        .drain(|bytes| virtio_console::send(bytes));
    emit_frame(&Frame::Dropped { count: dropped });
}

/// Thin wrapper for module-internal call sites that want to emit a
/// frame without naming `KernelSink`. Equivalent to constructing one
/// and calling `.emit`.
fn emit_frame(frame: &Frame<'_>) {
    KernelSink.emit(frame);
}
