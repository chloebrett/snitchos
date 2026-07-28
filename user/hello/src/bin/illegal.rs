//! Unhandled-user-trap probe (`workload=userspace-illegal`): emit a marker
//! through the granted `TelemetrySink`, then execute an instruction the machine
//! refuses. The kernel must **kill this process and carry on** — an unhandled
//! U-mode trap is the process's bug, not the kernel's.
//!
//! The sibling of `faulter`: there the page table says no, here the *decoder*
//! does. Both must be survivable, and before this probe existed only the page
//! fault was — any other exception from U-mode reached `trap_handler`'s
//! catch-all `panic!`, so one illegal instruction from any user program halted
//! the machine. That is what typing `1.5 + 1.5` at the Stitch REPL did: floats
//! compile to FP instructions, and FP traps as illegal while `sstatus.FS` is
//! Off. See `docs/floating-point-design.md`.
//!
//! `unimp` rather than an actual float, deliberately — the disposition of an
//! unhandled user trap is independent of floating point, and this probe should
//! keep gating it after FP lands and stops being illegal.

#![no_std]
#![no_main]

use snitchos_user::{entry, register_counter};

#[entry]
fn main() {
    // Prove we reached U-mode and the syscall path works before we die.
    register_counter("snitchos.illegal.marker").emit(99);

    // The probe. `unimp` is the spec's guaranteed-illegal encoding, so it traps
    // on QEMU, under snemu, and on the JH7110 alike. Never returns: the kernel
    // terminates us at the trap.
    // SAFETY: deliberately illegal; the kernel handles the trap by killing us.
    unsafe {
        core::arch::asm!("unimp", options(noreturn));
    }
}
