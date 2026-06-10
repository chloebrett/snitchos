//! Isolation probe (v0.7a Step 5e). Run via `workload=userspace-fault`.
//!
//! Emits one telemetry metric to prove it reached U-mode, then deliberately
//! reads a kernel high-half address. That page IS mapped in the process's
//! address space (the kernel high-half is shared into every user root) but
//! carries no `U` bit, so the load faults to S-mode — the kernel counts it.
//! Proves the `U`-bit firewall that v0.7b's capabilities build authority on
//! top of. Linked at the same fixed VA as `hello` (never loaded together).

#![no_std]
#![no_main]

use core::arch::global_asm;
use snitchos_abi::{Syscall, TELEMETRY_SINK_HANDLE};

global_asm!(include_str!("../start.S"));

/// A kernel high-half VA that is always mapped (the kernel image base, per
/// `kernel/linker.ld`) but carries no `U` bit. A U-mode load here faults.
const KERNEL_PROBE_VA: usize = 0xffff_ffff_8020_0000;

#[unsafe(no_mangle)]
pub extern "C" fn rust_main() -> ! {
    // Prove we reached U-mode (and the syscall path works from here too):
    // invoke the bootstrap TelemetrySink we were granted, like `hello`.
    let marker: usize = 99;
    // SAFETY: the kernel resolves a0 (our handle) against our CapTable,
    // emits a1, and returns.
    unsafe {
        core::arch::asm!(
            "ecall",
            in("a7") Syscall::Invoke as usize,
            in("a0") TELEMETRY_SINK_HANDLE,
            in("a1") marker,
            lateout("a0") _,
        );
    }

    // The isolation probe: read a kernel VA from U-mode. Mapped but not `U`,
    // so this faults to S-mode and never returns here — the kernel counts
    // the fault and parks this hart.
    // SAFETY: deliberately faulting; this load is expected to trap.
    unsafe {
        core::ptr::read_volatile(KERNEL_PROBE_VA as *const u64);
    }

    // Only reached if isolation is BROKEN (the read succeeded). The test
    // fails in that case (no fault counter ever appears).
    loop {
        core::hint::spin_loop();
    }
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}
