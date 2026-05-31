//! S-mode trap entry, exit, and dispatch.
//!
//! `trap_entry` (defined in `trap.S`) is the symbol pointed at by
//! `stvec`. The CPU jumps here on any trap (interrupt, exception,
//! environment call). Its only job is to save the trapped GPRs + sepc
//! + sstatus into a `TrapFrame` on the current stack, hand the frame
//! pointer to `trap_handler`, then restore everything and `sret`.

core::arch::global_asm!(include_str!("trap.S"));

/// Saved register state at trap entry. The assembly stores into these
/// fields in this order; the Rust dispatcher reads them by name.
///
/// `#[repr(C)]` guarantees byte-for-byte agreement with the
/// hand-written offsets in `trap.S`. Reorder fields here and the asm
/// will be wrong — keep them in sync.
#[repr(C)]
pub struct TrapFrame {
    pub ra: u64,       // x1   (offset 0)
    pub sp: u64,       // x2   (offset 8)
    pub gp: u64,       // x3
    pub tp: u64,       // x4
    pub t0: u64,       // x5
    pub t1: u64,
    pub t2: u64,
    pub s0: u64,       // x8
    pub s1: u64,
    pub a0: u64,       // x10
    pub a1: u64,
    pub a2: u64,
    pub a3: u64,
    pub a4: u64,
    pub a5: u64,
    pub a6: u64,
    pub a7: u64,
    pub s2: u64,       // x18
    pub s3: u64,
    pub s4: u64,
    pub s5: u64,
    pub s6: u64,
    pub s7: u64,
    pub s8: u64,
    pub s9: u64,
    pub s10: u64,
    pub s11: u64,
    pub t3: u64,       // x28
    pub t4: u64,
    pub t5: u64,
    pub t6: u64,
    pub sepc: u64,     // offset 248
    pub sstatus: u64,  // offset 256
}
