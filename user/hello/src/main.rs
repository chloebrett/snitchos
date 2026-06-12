//! The first SnitchOS userspace program. Invokes the `TelemetrySink` it was
//! granted (a real, kernel-validated capability), then deliberately reaches
//! for a handle it was never granted (refused), then exits. All crt0 / panic
//! / syscall plumbing lives in the `snitchos-user` runtime — this is just the
//! program logic.

#![no_std]
#![no_main]

use snitchos_user::{Startup, TelemetrySink, exit, yield_now};

#[unsafe(no_mangle)]
pub extern "C" fn rust_main(startup: Startup) -> ! {
    // Emit through the capability the kernel handed us at startup.
    let _ = startup.telemetry().emit(42);

    // Reach for authority we were never granted: handle 1, when our table
    // holds only the startup handle. The kernel refuses (and snitches the
    // denial) — the point of capabilities is that holding the integer isn't
    // enough, the kernel checks it against *our* table.
    let _ = TelemetrySink::from_raw_handle(1).emit(42);

    // Voluntarily give up the CPU. We can't call the kernel's `yield_now`
    // directly — this `ecall`s `Yield`, the kernel reschedules, and control
    // returns here on a later turn. Proves cooperative userspace works.
    yield_now();

    // Done — exit so the hart goes idle (`wfi`) instead of busy-spinning.
    exit();
}
