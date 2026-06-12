//! Span-quota probe (`workload=userspace-spanflood`): open spans with more
//! distinct names than `Process::MAX_SPAN_NAMES` (16). Each *new* name counts
//! against the per-process quota; once it's exhausted the kernel refuses the
//! open (`SyscallRefused{Quota}`) rather than leaking an unbounded number of
//! `'static` strings. Proves the quota bounds a misbehaving program without
//! the kernel panicking. crt0 / panic / syscalls come from the runtime.

#![no_std]
#![no_main]

use snitchos_user::Startup;

/// Twenty distinct names — four past the 16-name quota, so the last four opens
/// must be refused. Static literals (the runtime has no allocator to build
/// names at runtime).
const NAMES: &[&str] = &[
    "flood.00", "flood.01", "flood.02", "flood.03", "flood.04", "flood.05", "flood.06",
    "flood.07", "flood.08", "flood.09", "flood.10", "flood.11", "flood.12", "flood.13",
    "flood.14", "flood.15", "flood.16", "flood.17", "flood.18", "flood.19",
];

#[unsafe(no_mangle)]
pub extern "C" fn rust_main(startup: Startup) {
    let tracer = startup.tracer();
    for name in NAMES {
        // Open then immediately close (the guard drops at the end of the
        // statement). Each new name consumes one quota slot; once the quota is
        // spent, the open is refused and the guard is a no-op.
        let _ = tracer.span(name);
    }
    // Marker so the test can confirm the program ran the whole flood.
    let _ = startup.telemetry().emit(1);
}
