//! The first SnitchOS userspace program. Invokes the `TelemetrySink` it was
//! granted (a real, kernel-validated capability), then deliberately reaches
//! for a handle it was never granted (refused), then exits. All crt0 / panic
//! / syscall plumbing lives in the `snitchos-user` runtime — this is just the
//! program logic.

#![no_std]
#![no_main]

use snitchos_user::{TelemetrySink, exit};

#[unsafe(no_mangle)]
pub extern "C" fn rust_main() -> ! {
    // Emit through the capability we actually hold.
    let _ = TelemetrySink::bootstrap().emit(42);

    // Reach for authority we were never granted: handle 1, when our table
    // holds only handle 0. The kernel refuses (and snitches the denial) —
    // the point of capabilities is that holding the integer isn't enough.
    let _ = TelemetrySink::from_raw_handle(1).emit(42);

    // Done — exit so the hart goes idle (`wfi`) instead of busy-spinning.
    exit();
}
