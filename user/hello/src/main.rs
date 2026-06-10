//! The first SnitchOS userspace program. Runs in U-mode and makes exactly
//! one syscall: it **invokes the `TelemetrySink` capability** it was
//! granted at bootstrap (handle `TELEMETRY_SINK_HANDLE`) to emit a metric.
//! We hold an unforgeable handle and present it; the kernel validates it
//! against our table. (v0.7a did this ambiently — no handle, no check;
//! this is the capability-mediated version.)
//!
//! Linked position-dependent at a fixed low-half VA (see `user.ld`); the
//! kernel maps our segments there and `sret`s to `_start` (see `start.S`).

#![no_std]
#![no_main]

use core::arch::global_asm;
use snitchos_abi::{Syscall, TELEMETRY_SINK_HANDLE};

global_asm!(include_str!("start.S"));

/// Called from `_start` once the stack is set up. Invokes the telemetry
/// capability, then spins. The kernel handles the `ecall` and `sret`s back
/// here, where we loop forever (v0.7a has no exit syscall).
#[unsafe(no_mangle)]
pub extern "C" fn rust_main() -> ! {
    let value: usize = 42;
    // SAFETY: `ecall` traps to S-mode; the kernel resolves a0 (our handle)
    // against our CapTable, checks EMIT, emits a1 to the bound counter,
    // advances sepc past the ecall, and returns. a0 carries the result on
    // return (0 = ok), which we discard.
    unsafe {
        core::arch::asm!(
            "ecall",
            in("a7") Syscall::Invoke as usize,
            in("a0") TELEMETRY_SINK_HANDLE,
            in("a1") value,
            lateout("a0") _,
        );
    }

    // Now reach for authority we were never granted: invoke handle 1 — our
    // table holds only handle 0. The kernel refuses (and snitches the
    // denial); a0 comes back nonzero, which we discard. This is the point
    // of capabilities: holding the integer isn't enough, the kernel checks
    // it against *our* table.
    let ungranted_handle: usize = TELEMETRY_SINK_HANDLE + 1;
    // SAFETY: same trap contract; this invocation is expected to be denied.
    unsafe {
        core::arch::asm!(
            "ecall",
            in("a7") Syscall::Invoke as usize,
            in("a0") ungranted_handle,
            in("a1") value,
            lateout("a0") _,
        );
    }

    loop {
        core::hint::spin_loop();
    }
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}
