//! Span-quota probe (`workload=userspace-span-flood`): open spans with more
//! distinct names than `Process::MAX_SPAN_NAMES` (16). Each *new* name counts
//! against the per-process quota; once it's exhausted the kernel refuses the
//! open (`SyscallRefused{Quota}`) rather than leaking an unbounded number of
//! `'static` strings. Proves the quota bounds a misbehaving program without
//! the kernel panicking. crt0 / panic / syscalls come from the runtime.

#![no_std]
#![no_main]

use snitchos_user::{entry, register_counter, tracer};

/// Twenty distinct names — four past the 16-name quota, so the last four opens
/// must be refused. Static literals (no need to allocate distinct names).
const NAMES: &[&str] = &[
    "flood.00", "flood.01", "flood.02", "flood.03", "flood.04", "flood.05", "flood.06",
    "flood.07", "flood.08", "flood.09", "flood.10", "flood.11", "flood.12", "flood.13",
    "flood.14", "flood.15", "flood.16", "flood.17", "flood.18", "flood.19",
];

#[entry]
fn main() {
    let tracer = tracer();
    for name in NAMES {
        // Open then immediately close (the guard drops at the end of the
        // statement). Each new name consumes one quota slot; once the quota is
        // spent, the open is refused and the guard is a no-op.
        let _ = tracer.span(name);
    }
    // Marker so the test can confirm the program ran the whole flood.
    register_counter("snitchos.span_flood.marker").emit(1);
}
