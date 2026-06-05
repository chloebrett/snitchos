//! Inter-processor interrupts. The v0.6 SMP coordination primitive:
//! one hart "pokes" another via a software interrupt so the target
//! notices that state has changed.
//!
//! Two channels are folded into one mechanism:
//!
//!   - **The signal** — a software interrupt (S-mode software
//!     interrupt = `SSIP` bit in `sip`), raised on the target via
//!     SBI `send_ipi`. The interrupt itself carries no data; it
//!     just makes the target take a trap.
//!   - **The payload** — a bitfield of pending message types in
//!     the target's `PerHartData.ipi_pending`. The sender sets bits
//!     before sending the IPI; the target's trap handler reads +
//!     clears them and dispatches per bit.
//!
//! v0.6 step 7: only the `Wakeup` message type exists. Step 9 adds
//! `TlbShootdown`. The bitfield encoding keeps adding messages
//! mechanical (one bit + one match arm).
//!
//! ## Memory ordering
//!
//! Cross-hart synchronisation here — this is the first place in
//! the kernel that genuinely needs Release/Acquire. The contract
//! lives in `kernel::percpu`'s module docstring; the matching pair
//! is `fetch_or(_, Release)` on send and `swap(0, Acquire)` on
//! receive.

use core::sync::atomic::Ordering;

use crate::percpu::{self, PER_HART_DATA};
use crate::sbi;

/// Wake the target from `wfi` (or just acknowledge "you have new
/// work on your runqueue"). The receive-side handler is a no-op
/// today; the value is in the trap itself, which breaks `wfi`.
pub const IPI_WAKEUP: u32 = 1 << 0;

/// TLB shootdown request. Target runs `sfence.vma` (range read from
/// a separate per-hart slot, not yet wired). Reserved for step 9.
pub const IPI_TLB_SHOOTDOWN: u32 = 1 << 1;

/// Cumulative count of software interrupts dispatched by this
/// kernel. Drained by the heartbeat as
/// `snitchos.ipi.received_total`. `Relaxed`: counter.
pub static RECEIVED_TOTAL: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0);

/// Send `msg` to `target_hart`. Sets the pending bit *before*
/// raising the IPI so the target observes the bit when it processes
/// the interrupt.
pub fn send(target_hart: usize, msg: u32) {
    debug_assert!(target_hart < crate::percpu::MAX_HARTS);
    // Release: any state the sender wrote that the bit "announces"
    // (e.g., a future shootdown VA) is published before the target's
    // Acquire swap in `handle_pending` reads the bit.
    PER_HART_DATA[target_hart]
        .ipi_pending
        .fetch_or(msg, Ordering::Release);
    sbi::send_ipi(1u64 << target_hart, 0);
}

/// Trap-handler entry point for `SupervisorSoftwareInterrupt`.
/// Clears `SSIP`, atomically swaps out the pending bitflags, and
/// dispatches each set message.
///
/// Re-entry safety: clearing `SSIP` before swapping out the bitflags
/// means a new IPI raised after the swap re-triggers the trap on
/// next return; we'll handle the new bit then. A new IPI raised
/// *between* clearing `SSIP` and the swap is also safe: the bit is
/// still set in `ipi_pending`, our swap captures it, and the
/// re-raised `SSIP` causes a no-op re-entry.
pub fn handle_pending() {
    // Clear SSIP first so a new IPI arriving during dispatch
    // re-triggers the trap rather than being lost.
    unsafe {
        core::arch::asm!(
            "csrc sip, {}",
            in(reg) 1u64 << 1,  // SSIP
            options(nostack, preserves_flags),
        );
    }

    // Acquire: pair with the sender's Release on `fetch_or`. Any
    // payload the sender published before the bit is now visible.
    let bits = percpu::this_cpu().ipi_pending.swap(0, Ordering::Acquire);

    if bits & IPI_WAKEUP != 0 {
        // Wakeup is intentionally a no-op at the handler level —
        // the value is that the trap broke `wfi`. Future runqueue
        // wake check happens after this returns when the resumed
        // code re-evaluates "is there work."
    }
    if bits & IPI_TLB_SHOOTDOWN != 0 {
        // Reserved for step 9.
    }

    RECEIVED_TOTAL.fetch_add(1, Ordering::Relaxed);
}
