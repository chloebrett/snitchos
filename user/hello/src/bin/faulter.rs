//! Isolation probe (`workload=userspace-fault`): emit a marker through the
//! granted `TelemetrySink`, then read a kernel high-half VA from U-mode. That
//! page IS mapped in the process's address space (the kernel high-half is
//! shared into every user root) but carries no `U` bit, so the load faults to
//! S-mode and the kernel counts it — proving the `U`-bit firewall that
//! capabilities build authority on top of. Linked at the same fixed VA as
//! `hello` (never loaded together). crt0 / panic / syscalls come from the
//! `snitchos-user` runtime.

#![no_std]
#![no_main]

use snitchos_user::{entry, register_counter};

/// A kernel high-half VA that is always mapped (the kernel image base, per
/// `kernel/linker.ld`) but carries no `U` bit. A U-mode load here faults.
const KERNEL_PROBE_VA: usize = 0xffff_ffff_8020_0000;

#[entry]
fn main() {
    // Prove we reached U-mode and the syscall path works from here too.
    register_counter("snitchos.faulter.marker").emit(99);

    // The probe: a U-mode load of a kernel VA. Mapped but not `U`, so this
    // faults to S-mode and never returns here — the kernel counts the fault
    // and parks this hart. If isolation were BROKEN the read would succeed and
    // we'd return, and the runtime would exit the process.
    // SAFETY: deliberately faulting; the kernel handles the trap.
    unsafe {
        core::ptr::read_volatile(KERNEL_PROBE_VA as *const u64);
    }
}
