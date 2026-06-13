//! The first SnitchOS userspace program. Invokes the `TelemetrySink` it was
//! granted (a real, kernel-validated capability), then deliberately reaches
//! for a handle it was never granted (refused), then exits. All crt0 / panic
//! / syscall plumbing lives in the `snitchos-user` runtime — this is just the
//! program logic.

#![no_std]
#![no_main]

extern crate alloc;

use snitchos_user::{Startup, TelemetrySink, yield_now};

#[unsafe(no_mangle)]
pub extern "C" fn rust_main(startup: Startup) {
    // Emit through the capability the kernel handed us at startup.
    let _ = startup.telemetry().emit(42);

    // Open a span for the lifetime of the program, naming it with a
    // heap-allocated `format!` string — proving the userspace allocator works
    // (no allocator → this won't link). The RAII guard closes the span
    // (emitting `SpanEnd`) when `rust_main` returns; the runtime then `exit()`s.
    // The span stays open across the `yield` below (span-survives-yield).
    let name = alloc::format!("hello.{}", "work");
    let _span = startup.tracer().span(&name);

    // Reach for authority we were never granted: handle 1 (the `SpanSink`)
    // invoked as a telemetry sink → wrong object. The kernel refuses and
    // snitches the denial — holding the integer isn't authority.
    let _ = TelemetrySink::from_raw_handle(1).emit(42);

    // Voluntarily yield, mid-span. Control returns here on a later turn; the
    // span is still open, and closes when we return below.
    yield_now();
}
