//! The first SnitchOS userspace program. Runs in U-mode, makes exactly
//! one syscall — `EmitMetric` — and loops. The syscall is **ambient**:
//! nothing grants us the right to emit telemetry, we just `ecall`. v0.7b
//! makes that a capability invocation; this program is the "before".
//!
//! Linked position-dependent at a fixed low-half VA (see `user.ld`); the
//! kernel maps our segments there and `sret`s to `_start` (see `start.S`).

#![no_std]
#![no_main]

use core::arch::global_asm;
use snitchos_abi::Syscall;

global_asm!(include_str!("start.S"));

/// Called from `_start` once the stack is set up. Issues the telemetry
/// syscall, then spins. The kernel handles the `ecall` and `sret`s back
/// here, where we loop forever (v0.7a has no exit syscall).
#[unsafe(no_mangle)]
pub extern "C" fn rust_main() -> ! {
    let value: usize = 42;
    // SAFETY: `ecall` traps to S-mode; the kernel's syscall handler reads
    // a7/a0, emits the metric, advances sepc past the ecall, and returns.
    // a0 carries the syscall result on return, which we discard.
    unsafe {
        core::arch::asm!(
            "ecall",
            in("a7") Syscall::EmitMetric as usize,
            in("a0") value,
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
