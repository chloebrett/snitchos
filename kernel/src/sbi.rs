//! Thinnest possible SBI shim. `SnitchOS` calls SBI for things the kernel cannot
//! (or should not) do directly: IPI delivery (`send_ipi`), secondary bring-up
//! (`hart_start`), and arming the supervisor timer (`set_timer`). The clock is
//! SBI-only — it reads `time` via `rdtime` but arms through `set_timer`, not a
//! direct `stimecmp` write, so it runs on cores without Sstc (the JH7110 U74).
//! Console is virtio, not SBI.
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

/// SBI TIME extension id ("TIME" packed as ASCII).
const EID_TIME: u64 = 0x5449_4D45;

/// Arm the supervisor timer to fire at absolute time `deadline` (in `time`-CSR
/// units). SBI TIME extension, FID 0 (`sbi_set_timer`) — portable to cores without
/// Sstc (the JH7110 U74): the firmware programs the timer and delivers `STIP`.
/// Setting a future deadline also clears any pending timer interrupt (SBI spec).
/// Panics on error: a kernel that can't arm its heartbeat clock can't run.
pub fn set_timer(deadline: u64) {
    let error: i64;
    unsafe {
        asm!(
            "ecall",
            in("a7") EID_TIME,
            in("a6") 0_u64,            // FID 0 = sbi_set_timer
            inlateout("a0") deadline => error,
            // SBI returns `sbiret { error, value }` in a0 **and a1**, so a1 is
            // clobbered by every ecall even when we ignore the value. Declaring it
            // is not optional: without this the compiler may keep a live value in
            // a1 across the call and read back the firmware's return instead. That
            // is exactly what happened — a release build parked the `PER_HART_DATA`
            // base in a1 here, got 0 back, and the trap handler's per-hart counter
            // stored to 0x40. See the SBI-clobber callout in
            // plans/visionfive2-port.md.
            lateout("a1") _,
            options(nostack),
        );
    }
    assert!(error == 0, "sbi_set_timer failed: error={error}");
}

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
            // `inlateout … => _`, not `in`: a1 is an argument *and* a return slot.
            // `in` would promise the compiler a1 still holds `hart_mask_base` after
            // the ecall, which the firmware has already overwritten.
            inlateout("a1") hart_mask_base => _,
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
            // Argument *and* return slot — see `send_ipi`. a2 stays `in`: SBI
            // preserves every register except a0/a1.
            inlateout("a1") start_addr => _,
            in("a2") opaque,
            options(nostack),
        );
    }
    error
}
