//! Thinnest possible SBI shim. `SnitchOS` only calls SBI for things
//! the kernel cannot do directly — today that's IPI delivery
//! (`send_ipi`). The clock uses Sstc CSRs directly, no SBI; console
//! is virtio not SBI; no HSM yet (secondary harts in step 8 will
//! grow this module).
//!
//! Calling convention (SBI 1.0):
//!   - a7 = EID (extension id)
//!   - a6 = FID (function id)
//!   - a0..a5 = args
//!   - on return: a0 = error, a1 = value
//!
//! We only check the error code where the caller could plausibly do
//! something about it. `send_ipi` failures today are panic-worthy:
//! a kernel that can't IPI can't continue with SMP.

use core::arch::asm;

/// SBI IPI extension id ("sPI" packed as ASCII).
const EID_IPI: u64 = 0x735049;

/// SBI HSM (Hart State Management) extension id ("HSM" packed as ASCII).
const EID_HSM: u64 = 0x48534D;

/// Send an inter-processor interrupt to a set of harts.
///
/// `hart_mask`: bitmask of harts to target, where bit `i` selects
/// `hart_mask_base + i`.
/// `hart_mask_base`: starting hart id for the mask.
///
/// To target hart `h` exactly: `send_ipi(1 << h, 0)` (mask=`1<<h`,
/// base=0) or `send_ipi(1, h)` (mask=`1`, base=`h`). Both work; the
/// first form is uniform across all targets up to 64 harts.
///
/// Panics if SBI returns an error. The kernel has no way to recover
/// from "IPI delivery is broken."
pub fn send_ipi(hart_mask: u64, hart_mask_base: u64) {
    let error: i64;
    unsafe {
        asm!(
            "ecall",
            in("a7") EID_IPI,
            in("a6") 0_u64,           // FID 0 = sbi_send_ipi
            inlateout("a0") hart_mask => error,
            in("a1") hart_mask_base,
            options(nostack),
        );
    }
    assert!(error == 0, "sbi_send_ipi failed: error={error}");
}

/// Wake a parked hart and jump it to `start_addr` (a *physical*
/// address, since the target starts with MMU off). `opaque` is
/// passed as `a1` to the target on entry.
///
/// SBI HSM extension, FID 0 (`sbi_hart_start`).
///
/// Returns the SBI error code: 0 on success, non-zero on failure.
/// Caller decides how to react — for v0.6 step 8 the kernel panics
/// because there's nothing to do if hart 1 can't start.
pub fn hart_start(hartid: u64, start_addr: u64, opaque: u64) -> i64 {
    let error: i64;
    unsafe {
        asm!(
            "ecall",
            in("a7") EID_HSM,
            in("a6") 0_u64,  // FID 0 = sbi_hart_start
            inlateout("a0") hartid => error,
            in("a1") start_addr,
            in("a2") opaque,
            options(nostack),
        );
    }
    error
}
