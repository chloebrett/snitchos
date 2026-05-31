//! S-mode trap entry, exit, and dispatch.
//!
//! `trap_entry` (defined in `trap.S`) is the symbol pointed at by
//! `stvec`. The CPU jumps here on any trap (interrupt, exception,
//! environment call). Its only job is to save the trapped GPRs + sepc
//! + sstatus into a `TrapFrame` on the current stack, hand the frame
//! pointer to `trap_handler`, then restore everything and `sret`.

core::arch::global_asm!(include_str!("trap.S"));

use core::arch::asm;

/// Saved register state at trap entry. The assembly stores into these
/// fields in this order; the Rust dispatcher reads them by name.
///
/// `#[repr(C)]` guarantees byte-for-byte agreement with the
/// hand-written offsets in `trap.S`. Reorder fields here and the asm
/// will be wrong — keep them in sync.
#[repr(C)]
pub struct TrapFrame {
    pub ra: u64, // x1   (offset 0)
    pub sp: u64, // x2   (offset 8)
    pub gp: u64, // x3
    pub tp: u64, // x4
    pub t0: u64, // x5
    pub t1: u64,
    pub t2: u64,
    pub s0: u64, // x8
    pub s1: u64,
    pub a0: u64, // x10
    pub a1: u64,
    pub a2: u64,
    pub a3: u64,
    pub a4: u64,
    pub a5: u64,
    pub a6: u64,
    pub a7: u64,
    pub s2: u64, // x18
    pub s3: u64,
    pub s4: u64,
    pub s5: u64,
    pub s6: u64,
    pub s7: u64,
    pub s8: u64,
    pub s9: u64,
    pub s10: u64,
    pub s11: u64,
    pub t3: u64, // x28
    pub t4: u64,
    pub t5: u64,
    pub t6: u64,
    pub sepc: u64,    // offset 248
    pub sstatus: u64, // offset 256
}

#[unsafe(no_mangle)]
pub extern "C" fn trap_handler(_frame: *mut TrapFrame) {
    let scause: u64;
    unsafe {
        asm!("csrr {}, scause", out(reg) scause);
    }
    let is_interrupt = (scause >> 63) & 1 == 1;
    let code = scause & !(1u64 << 63);
    panic!("trap! is_interrupt={is_interrupt}, code={code}, scause={scause:#x}");
}

/// Install our `trap_entry` (from `trap.S`) as the S-mode trap vector.
/// After this returns, every trap (exception or interrupt) routes to
/// our handler. Call once, at boot, before anything that might trap.
///
/// # Safety
///
/// No other code should be relying on the previous `stvec` value.
/// At first boot stvec is undefined; we're writing it for the first time.
pub unsafe fn set_trap_vector() {
    unsafe extern "C" {
        fn trap_entry();
    }
    let addr = trap_entry as *const () as usize;
    unsafe {
        asm!(
          "csrw stvec, {}", in(reg) addr
        );
    }
}
