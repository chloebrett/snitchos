//! Kernel-side telemetry: timestamp source, string interning, span ID
//! allocation, and the `span!` macro that emits `Frame::SpanStart` /
//! `Frame::SpanEnd` around a block of code.
//!
//! All frames go out the virtio-console (`virtio_console::send`). The
//! host-reader decodes them.

use protocol::{Frame, MetricKind, StringId};

use kernel_core::clock::Clock;
use kernel_core::intern::InternTable;
use kernel_core::sink::FrameSink;
use kernel_core::span::SpanRegistry;

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

static INTERN_TABLE: spin::Mutex<InternTable> = spin::Mutex::new(InternTable::new());

/// Adapter that lets `kernel_core` types (which only know `FrameSink`)
/// emit through this module's `emit_frame`. Zero-sized; constructed
/// fresh per call. Stays simple until step 8 of the carve-out, which
/// folds the virtio-console + pre-init dispatch into this impl.
struct KernelSink;

impl FrameSink for KernelSink {
    fn emit(&mut self, frame: &Frame<'_>) {
        emit_frame(frame);
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
// `SpanRegistry` (in kernel-core) owns the id-allocation + parent-stack
// bookkeeping; this module owns the wire emit (SpanStart on open,
// SpanEnd on Drop) and the static instance.

/// Per-hart span registry. Single-hart for v0.1; SMP will need one per
/// CPU plus id-space partitioning (see plans/scaling-corners.md).
static SPAN_REGISTRY: SpanRegistry = SpanRegistry::new();

/// RAII guard returned by `span_start`. Drop emits `SpanEnd` and
/// restores the parent-stack to the parent.
///
/// Known weakness: `mem::forget(span)` skips Drop, leaking the span
/// (no SpanEnd on the wire) and leaving the registry pointing at this
/// span forever. The `span!` macro avoids handing the user a name-bound
/// guard they could forget.
pub struct Span(kernel_core::span::SpanOpen);

impl Drop for Span {
    fn drop(&mut self) {
        emit_frame(&Frame::SpanEnd {
            id: self.0.id,
            t: timestamp(),
        });
        SPAN_REGISTRY.close(&self.0);
    }
}

/// Open a span named `name`. Returns a `Span` guard whose `Drop` will
/// emit `SpanEnd`. Nesting is automatic from Rust scopes.
pub fn span_start(name: &'static str) -> Span {
    let name_id = register_or_lookup(name);
    let open = SPAN_REGISTRY.open();
    emit_frame(&Frame::SpanStart {
        id: open.id,
        parent: open.parent,
        name_id,
        t: timestamp(),
    });
    Span(open)
}

// --- Frame emission, with pre-init buffering ---

/// Bytes we buffer up before `virtio_console::init` has completed.
/// 1 KiB is plenty for all the boot-phase spans + their StringRegisters
/// (each frame is ~10–30 bytes).
const PRE_INIT_BYTES: usize = 1024;

struct PreInit {
    bytes: [u8; PRE_INIT_BYTES],
    len: usize,
    /// Count of frames that couldn't fit in the buffer.
    dropped: u32,
}

static PRE_INIT_BUFFER: spin::Mutex<PreInit> = spin::Mutex::new(PreInit {
    bytes: [0; PRE_INIT_BYTES],
    len: 0,
    dropped: 0,
});

/// Flush any frames buffered while the virtio-console was still
/// initializing. Call this exactly once, right after
/// `virtio_console::init` succeeds.
///
/// Always follows with a `Frame::Dropped { count }` — the host treats
/// this as a positive "buffer flushed, here's the loss count"
/// checkpoint. `count == 0` means no frames were lost.
pub fn flush_pre_init() {
    let dropped = {
        let mut buffer = PRE_INIT_BUFFER.lock();
        if buffer.len > 0 {
            virtio_console::send(&buffer.bytes[..buffer.len]);
            buffer.len = 0;
        }
        let dropped = buffer.dropped;
        buffer.dropped = 0;
        dropped
        // Lock drops here.
    };
    emit_frame(&Frame::Dropped { count: dropped });
}

/// Encode a single frame into a stack buffer and ship it out the
/// virtio-console — or, if the console isn't up yet, append to the
/// pre-init buffer so it can be flushed later.
///
/// The 128-byte buffer is sized for span/event/metric frames and
/// short `StringRegister`s (the longest name we register is ~30 chars).
///
/// Known weaknesses:
/// - **Buffer overflow drops frames.** Encode failure (frame > 128 B)
///   or pre-init buffer full → frame silently dropped (or, in the
///   pre-init case, the `overflow` flag fires and `flush_pre_init`
///   emits a `Dropped` to tell the host).
fn emit_frame(frame: &Frame<'_>) {
    let mut buf = [0u8; 128];
    let Ok(bytes) = postcard::to_slice(frame, &mut buf) else {
        return;
    };

    if virtio_console::CONSOLE.get().is_some() {
        virtio_console::send(bytes);
    } else {
        // Append to pre-init buffer; count drops if we don't fit.
        let mut buffer = PRE_INIT_BUFFER.lock();
        let start = buffer.len;
        let end = start + bytes.len();
        if end <= PRE_INIT_BYTES {
            buffer.bytes[start..end].copy_from_slice(bytes);
            buffer.len = end;
        } else {
            buffer.dropped = buffer.dropped.saturating_add(1);
        }
    }
}
