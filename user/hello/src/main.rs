//! The first SnitchOS userspace program. Invokes the `TelemetrySink` it was
//! granted (a real, kernel-validated capability), then deliberately reaches
//! for a handle it was never granted (refused), then exits. All crt0 / panic
//! / syscall plumbing lives in the `snitchos-user` runtime — this is just the
//! program logic.

#![no_std]
#![no_main]

extern crate alloc;

use snitchos_user::{TelemetrySink, entry, telemetry, tracer, yield_now};

// The program entry: a plain `main`. `#[entry]` supplies the
// `#[unsafe(no_mangle)] extern "C"` decoration the runtime links against, so
// the program reads like a normal one. The runtime publishes the startup caps
// before calling us — reach them via the `telemetry()` / `tracer()`
// accessors — and `exit()`s after we return.
#[entry]
fn main() {
    // Emit through the capability the kernel handed us at startup.
    let _ = telemetry().emit(42);

    // A std-shaped `println!` — goes through the facade → `DebugWrite` syscall
    // → a snitched `Log` frame on the wire.
    snitchos_std::println!("hello from userspace");

    // Open a span for the lifetime of the program, naming it with a
    // heap-allocated `format!` string — proving the userspace allocator works
    // (no allocator → this won't link). The RAII guard closes the span
    // (emitting `SpanEnd`) when `main` returns; the runtime then `exit()`s.
    // The span stays open across the `yield` below (span-survives-yield).
    let name = alloc::format!("hello.{}", "work");
    let _span = tracer().span(&name);

    // Reach for authority we were never granted: handle 1 (the `SpanSink`)
    // invoked as a telemetry sink → wrong object. The kernel refuses and
    // snitches the denial — holding the integer isn't authority.
    let _ = TelemetrySink::from_raw_handle(1).emit(42);

    // Voluntarily yield, mid-span. Control returns here on a later turn; the
    // span is still open, and closes when we return below.
    yield_now();
}
