//! `workload=probe`: the per-process naming demos (debt #2 + its span follow-on).
//!
//! Metrics: the program names *its own* metric — the kernel doesn't know it ahead
//! of time. It registers `snitchos.probe.custom` (a gauge) through its bootstrap
//! `TelemetrySink`, emits a sample through the returned handle, then deliberately
//! emits through a handle it never registered: the kernel must **refuse** that
//! (`SyscallRefused`), proving the per-process metric table is the forgery
//! boundary.
//!
//! Spans: it opens a span named after a *kernel* span (`kernel.heartbeat`).
//! Per-process span-name scoping means the kernel hands it its **own** distinct
//! `StringId` for that name — never the kernel's — so it cannot emit a span under
//! the kernel's identity. The `span-name-not-poisonable` scenario asserts two
//! distinct `StringRegister`s for the name.

#![no_std]
#![no_main]

use snitchos_std::time::Instant;
use snitchos_user::{Metric, clock_freq, clock_now, entry, register_gauge, tracer};

/// A span name the *kernel* also uses — the poisoning probe. With per-process
/// scoping we get a fresh id for it, distinct from the kernel's.
const KERNEL_SPAN_NAME: &str = "kernel.heartbeat";

/// The value emitted to the registered metric — a recognisable sentinel the
/// integration test asserts on the wire.
const SAMPLE: i64 = 42;

/// A handle the process never registered. Its metric table holds only the one
/// metric registered below (handle 0), so this names nothing — the emit must be
/// refused, not silently land on some metric.
const UNREGISTERED_HANDLE: usize = 7;

#[entry]
fn main() {
    // Register a process-named gauge and emit to it through the returned handle.
    // The name crosses the kernel boundary once, here.
    register_gauge("snitchos.probe.custom").emit(SAMPLE);

    // Reach for a metric we never registered — the kernel refuses (the snitch is
    // the point), and no sample is emitted.
    Metric::from_raw_handle(UNREGISTERED_HANDLE).emit(99);

    // Open a span named after a kernel span. Per-process scoping gives us our own
    // `StringId`, distinct from the kernel's — no span-name poisoning. The guard
    // closes it on return.
    let _span = tracer().span(KERNEL_SPAN_NAME);

    // Report the platform timebase the runtime learned via the `ClockFreq`
    // syscall — the rate `std::time::Instant` divides tick deltas by. The itest
    // asserts it equals the DTB timebase, proving no hardcoded clock rate in
    // userspace.
    register_gauge("snitchos.probe.timebase_hz").emit(clock_freq() as i64);

    // Exercise `Instant` end to end: time a bounded spin (guaranteed to burn
    // >= 1000 ticks so the elapsed `Duration` is non-zero) and emit its nanos.
    let start = Instant::now();
    let t0 = clock_now();
    while clock_now() - t0 < 1000 {
        core::hint::spin_loop();
    }
    register_gauge("snitchos.probe.elapsed_nanos").emit(start.elapsed().as_nanos() as i64);
}
