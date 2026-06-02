//! Kernel-side telemetry: timestamp source, string interning, span ID
//! allocation, and the `span!` macro that emits `Frame::SpanStart` /
//! `Frame::SpanEnd` around a block of code.
//!
//! All frames go out the virtio-console (`virtio_console::send`). The
//! host-reader decodes them.

use core::sync::atomic::{AtomicU64, Ordering};

use protocol::{Frame, MetricKind, SpanId, StringId};

use kernel_core::clock::Clock;

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

/// Maximum number of unique strings we can register. Sized for v0.1 —
/// kernel.boot, heartbeat, a handful of init phases. 64 is plenty.
const MAX_INTERNED: usize = 64;

/// An entry in the intern table. Beyond the name itself we also track
/// whether the host has been told this name's metric type via
/// `Frame::MetricRegister` — avoids emitting duplicate type declarations
/// when `register_counter`/`gauge`/`histogram` is called repeatedly.
#[derive(Copy, Clone)]
struct InternEntry {
    name: &'static str,
    metric_registered: bool,
}

struct InternTable {
    entries: [Option<InternEntry>; MAX_INTERNED],
    next_id: u32,
}

static INTERN_TABLE: spin::Mutex<InternTable> = spin::Mutex::new(InternTable {
    entries: [None; MAX_INTERNED],
    next_id: 0,
});

/// Look up `name` in the intern table, inserting a new entry if absent.
/// Returns `(id, is_new)`: `is_new` is true iff this call allocated a
/// fresh slot, so the caller knows whether to emit a `StringRegister`.
///
/// Equality is by **pointer**, not value. Two `&'static str`s with the
/// same characters from different crates would be registered twice —
/// fine for v0.1 (single-crate kernel), worth fixing if userspace ever
/// registers names.
///
/// Panics if the table is full. Programmer error: bump `MAX_INTERNED`
/// or stop creating unique names.
fn lookup_or_insert(table: &mut InternTable, name: &'static str) -> (StringId, bool) {
    for (i, entry) in table.entries.iter().enumerate() {
        if let Some(e) = entry {
            if e.name.as_ptr() == name.as_ptr() {
                return (StringId(i as u32), false);
            }
        }
    }

    let id = table.next_id;
    let slot = id as usize;
    if slot >= MAX_INTERNED {
        panic!(
            "tracing: intern table full ({} entries); bump MAX_INTERNED",
            MAX_INTERNED,
        );
    }
    table.entries[slot] = Some(InternEntry {
        name,
        metric_registered: false,
    });
    table.next_id = id + 1;
    (StringId(id), true)
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
    let mut table = INTERN_TABLE.lock();
    let (id, is_new) = lookup_or_insert(&mut table, name);
    if is_new {
        emit_frame(&Frame::StringRegister { id, value: name });
    }
    id
}

/// Number of names currently registered in the intern table. Exposed
/// as a metric (`snitchos.intern.strings_used`).
pub fn intern_count() -> u32 {
    INTERN_TABLE.lock().next_id
}

/// Register a metric with its kind. If the name is new, both
/// `StringRegister` and `MetricRegister` are emitted; if the name was
/// previously registered but never as a metric, only `MetricRegister`
/// is emitted; if both have been registered before, nothing happens.
///
/// Calling `register_counter("foo")` and then `register_gauge("foo")`
/// is a programmer error — the second call won't re-declare the kind,
/// so the host will see `"foo"` as a Counter forever. Don't do that.
fn register_metric(name: &'static str, kind: MetricKind) -> StringId {
    let mut table = INTERN_TABLE.lock();
    let (id, is_new) = lookup_or_insert(&mut table, name);
    if is_new {
        emit_frame(&Frame::StringRegister { id, value: name });
    }
    // SAFETY: lookup_or_insert guarantees the slot is populated.
    let entry = table.entries[id.0 as usize].as_mut().unwrap();
    if !entry.metric_registered {
        entry.metric_registered = true;
        emit_frame(&Frame::MetricRegister { name_id: id, kind });
    }
    id
}

/// Register `name` as a Counter metric. Returns its `StringId` for use
/// with `emit_metric`. Idempotent — safe to call every iteration of a
/// loop; the host only sees one `MetricRegister`.
pub fn register_counter(name: &'static str) -> StringId {
    register_metric(name, MetricKind::Counter)
}

/// Register `name` as a Gauge metric.
pub fn register_gauge(name: &'static str) -> StringId {
    register_metric(name, MetricKind::Gauge)
}

/// Register `name` as a Histogram metric.
pub fn register_histogram(name: &'static str) -> StringId {
    register_metric(name, MetricKind::Histogram)
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

/// Monotonic span ID source. Starts at 1 so that `SpanId(0)` can act as
/// the "no parent" sentinel.
///
/// v0.1: single-hart, single counter. v0.5+ SMP will partition the u64
/// space per-CPU to avoid cross-hart coordination (see design doc:
/// "per-CPU-partitioned u64 counter").
static SPAN_ID_COUNTER: AtomicU64 = AtomicU64::new(1);

/// The id of the currently open innermost span on this hart, or 0
/// (`SpanId(0)`) at the root level. New spans read this to find their
/// parent; their `Drop` restores the previous value.
static CURRENT_SPAN: AtomicU64 = AtomicU64::new(0);

/// RAII guard returned by `span_start`. Drops emit `SpanEnd` and
/// restore `CURRENT_SPAN` to the parent.
///
/// Known weaknesses:
/// - `mem::forget(span)` skips `Drop`, leaking the span (no SpanEnd
///   on the wire) and corrupting `CURRENT_SPAN` for everything after.
///   The `span!` macro avoids handing the user a name-bound guard
///   they could forget.
pub struct Span {
    id: SpanId,
    parent: SpanId,
}

impl Drop for Span {
    fn drop(&mut self) {
        emit_frame(&Frame::SpanEnd {
            id: self.id,
            t: timestamp(),
        });
        // Restore CURRENT_SPAN to the parent so any later sibling span
        // sees the correct parent.
        CURRENT_SPAN.store(self.parent.0, Ordering::Relaxed);
    }
}

/// Open a span named `name`. Returns a `Span` guard whose `Drop` will
/// emit `SpanEnd`. Nesting is automatic from Rust scopes.
pub fn span_start(name: &'static str) -> Span {
    let parent = SpanId(CURRENT_SPAN.load(Ordering::Relaxed));
    let id = SpanId(SPAN_ID_COUNTER.fetch_add(1, Ordering::Relaxed));
    CURRENT_SPAN.store(id.0, Ordering::Relaxed);
    let name_id = register_or_lookup(name);
    emit_frame(&Frame::SpanStart {
        id,
        parent,
        name_id,
        t: timestamp(),
    });
    Span { id, parent }
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
