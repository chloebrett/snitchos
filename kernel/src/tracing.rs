//! Kernel-side telemetry: timestamp source, string interning, span ID
//! allocation, and the `span!` macro that emits `Frame::SpanStart` /
//! `Frame::SpanEnd` around a block of code.
//!
//! All frames go out the virtio-console (`virtio_console::send`). The
//! host-reader decodes them.

use core::sync::atomic::{AtomicU64, Ordering};

use protocol::{Frame, SpanId, StringId};

use crate::virtio_console;

/// Read the RISC-V `time` CSR — a monotonically increasing 64-bit cycle
/// counter clocked at the rate the DTB reports as `timebase-frequency`.
///
/// Convert to seconds on the host side via `ticks / timebase_hz`; the
/// kernel never does the division (avoids overflow in the
/// `ticks * 10^9 / hz` form, and keeps the math out of the hot path).
pub fn timestamp() -> u64 {
    let t: u64;
    unsafe {
        core::arch::asm!("rdtime {t}", t = out(reg) t);
    }
    t
}

// --- String intern table ---

/// Maximum number of unique strings we can register. Sized for v0.1 —
/// kernel.boot, heartbeat, a handful of init phases. 64 is plenty.
const MAX_INTERNED: usize = 64;

struct InternTable {
    entries: [Option<&'static str>; MAX_INTERNED],
    next_id: u32,
}

static INTERN_TABLE: spin::Mutex<InternTable> = spin::Mutex::new(InternTable {
    entries: [None; MAX_INTERNED],
    next_id: 0,
});

/// Look up `name` in the intern table. If it's new, assign a fresh
/// `StringId` and emit a `Frame::StringRegister` so the host learns
/// the mapping. If it's already known, return the existing id without
/// emitting.
///
/// Equality is by **pointer**, not value. Two `&'static str`s with the
/// same characters from different crates would be registered twice —
/// fine for v0.1 (single-crate kernel), worth fixing if userspace ever
/// registers names.
///
/// Known weaknesses:
/// - **Panics if the table is full.** Programmer error: bump
///   `MAX_INTERNED` or stop creating unique names.
/// - **Holds the table lock during the wire emit.** Locking order is
///   intern → virtio_console::CONSOLE. As long as nothing else takes
///   them in the opposite order we're fine; v0.1 has no other lockers.
pub fn register_or_lookup(name: &'static str) -> StringId {
    let mut table = INTERN_TABLE.lock();

    // Scan existing entries — pointer equality, O(N) but N is small.
    for (i, entry) in table.entries.iter().enumerate() {
        if let Some(s) = entry {
            if s.as_ptr() == name.as_ptr() {
                return StringId(i as u32);
            }
        }
    }

    // New string. Assign id, store, emit.
    let id = table.next_id;
    let slot = id as usize;
    if slot >= MAX_INTERNED {
        panic!(
            "tracing: intern table full ({} entries); bump MAX_INTERNED",
            MAX_INTERNED,
        );
    }
    table.entries[slot] = Some(name);
    table.next_id = id + 1;

    emit_frame(&Frame::StringRegister {
        id: StringId(id),
        value: name,
    });

    StringId(id)
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

// --- Frame emission ---

/// Encode a single frame into a stack buffer and ship it out the
/// virtio-console. The 128-byte buffer is sized for span/event/metric
/// frames and short `StringRegister`s (the longest name we register
/// is ~30 chars).
///
/// Known weaknesses:
/// - Buffer overflow → encode failure → frame silently dropped. v0.1
///   accepts this; longer strings should bump the buffer size.
fn emit_frame(frame: &Frame<'_>) {
    let mut buf = [0u8; 128];
    if let Ok(bytes) = postcard::to_slice(frame, &mut buf) {
        virtio_console::send(bytes);
    }
}
