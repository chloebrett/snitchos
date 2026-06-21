//! `workload=probe`: the userspace-defined-metrics demo (debt #2).
//!
//! The program names *its own* metric — the kernel doesn't know it ahead of
//! time. It registers `snitchos.probe.custom` (a gauge) through its bootstrap
//! `TelemetrySink` capability, gets back an opaque handle, and emits a sample
//! through it. Then it deliberately emits through a handle it never registered:
//! the kernel must **refuse** that (`SyscallRefused`), proving the per-process
//! metric table is the forgery boundary — a process can't emit to a metric (or
//! the kernel's own telemetry) it didn't name.

#![no_std]
#![no_main]

use snitchos_user::{Metric, entry, register_gauge};

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
}
