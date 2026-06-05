//! S-mode trap entry, exit, and dispatch.
//!
//! `trap_entry` (defined in `trap.S`) is the symbol pointed at by
//! `stvec`. The CPU jumps here on any trap (interrupt, exception,
//! environment call). Its only job is to save the trapped GPRs + sepc
//! + sstatus into a `TrapFrame` on the current stack, hand the frame
//! pointer to `trap_handler`, then restore everything and `sret`.

core::arch::global_asm!(include_str!("trap.S"));

use core::arch::asm;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use kernel_core::clock::Clock;
use kernel_core::trap::{TrapCause, decode_scause};

// ## Memory ordering note for the timer-IRQ statics below
//
// `TICK_PENDING` (set by ISR, read by main) and `LAST_IRQ_DURATION`
// (written by ISR, read by main after observing TICK_PENDING) form a
// classic publication pattern. Across harts that pattern needs
// `Release` on the store side and `Acquire` on the load side.
//
// Here both ends always run on the same hart: the timer interrupt is
// a local CSR-driven IRQ, taken on whichever hart's `stimecmp`
// expired. Trap return synchronises *all* of the handler's memory
// ops with the resumed thread by hardware — the resumed thread
// cannot observe the bit flip without also observing the duration
// write. Therefore `Relaxed` is correct.
//
// Under v0.9 preemption + multi-hart, this argument still holds
// (each hart's timer IRQ still runs on that hart). If we ever move
// to a single global heartbeat collected from one designated hart,
// these become genuinely cross-hart and need Release/Acquire.

/// How many ticks between timer interrupts. Set by `init_timer` from
/// the DTB timebase; the IRQ handler reads it to arm the next deadline.
/// `Relaxed`: init-once, then read forever — no payload to publish.
pub static TIMER_INTERVAL_TICKS: AtomicU64 = AtomicU64::new(0);

/// Set by the timer IRQ handler; the main thread polls + clears.
/// `Relaxed`: same-CPU IRQ handoff — trap return sequences memory.
pub static TICK_PENDING: AtomicBool = AtomicBool::new(false);

/// Duration of the most recent timer IRQ in ticks. The IRQ handler
/// measures `rdtime` at entry and exit; the main thread reads this
/// after wake and emits a histogram observation. (We can't emit
/// telemetry from the IRQ itself — would deadlock on the intern /
/// virtio_console mutexes.)
/// `Relaxed`: same-CPU IRQ handoff — see block comment above.
pub static LAST_IRQ_DURATION: AtomicU64 = AtomicU64::new(0);

/// SSTC-based clock: reads `time` CSR directly, writes `stimecmp`
/// (CSR 0x14d) to arm. No SBI round-trip. Implements
/// `kernel_core::clock::Clock`.
pub struct SstcClock;

impl Clock for SstcClock {
    fn now(&self) -> u64 {
        let t: u64;
        unsafe {
            asm!("rdtime {}", out(reg) t);
        }
        t
    }
    fn arm(&self, deadline: u64) {
        unsafe {
            asm!("csrw 0x14d, {}", in(reg) deadline);
        }
    }
}

/// The clock used by the IRQ handler and boot-time timer setup. A
/// single concrete instance lives here so the handler doesn't need to
/// take a `&dyn Clock` (no allocator, and the cost of dynamic dispatch
/// in an IRQ is silly when we only ever have one impl).
pub const CLOCK: SstcClock = SstcClock;

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
    match decode_scause(scause) {
        TrapCause::SupervisorTimerInterrupt => handle_timer(),
        other => panic!("unhandled trap: {other:?} (scause={scause:#x})"),
    }
}

/// Timer IRQ handler. Kept tiny: measure duration, arm the next
/// deadline (which acks the current pending bit), then set a flag so
/// the main thread knows to do the real work. **No locks taken here**
/// — the main thread owns all telemetry emission.
fn handle_timer() {
    let start = CLOCK.now();
    let interval = TIMER_INTERVAL_TICKS.load(Ordering::Relaxed);
    CLOCK.arm(start + interval);
    TICK_PENDING.store(true, Ordering::Relaxed);
    let end = CLOCK.now();
    LAST_IRQ_DURATION.store(end.wrapping_sub(start), Ordering::Relaxed);
}

/// One-time timer setup: set the interval, arm the first deadline,
/// enable interrupts. Call once from kmain after the trap vector is
/// installed.
///
/// # Safety
///
/// Trap vector must be installed (`set_trap_vector`) before this —
/// otherwise the first timer interrupt jumps to garbage.
pub unsafe fn init_timer(interval_ticks: u64) {
    TIMER_INTERVAL_TICKS.store(interval_ticks, Ordering::Relaxed);
    CLOCK.arm(CLOCK.now() + interval_ticks);
    unsafe { enable_timer_interrupts() };
}

/// Enable S-mode timer interrupts. Sets the per-source enable bit
/// (`sie.STIE`) and the global S-mode interrupt enable (`sstatus.SIE`).
///
/// Order matters: set the per-source mask before the global enable,
/// so a stale pending interrupt from another source can't fire on us
/// the instant we flip SIE.
///
/// # Safety
///
/// After this returns, timer interrupts will be delivered to our
/// trap handler whenever `time >= stimecmp`. Caller must ensure the
/// trap vector is installed and the handler is ready to deal with
/// them.
pub unsafe fn enable_timer_interrupts() {
    unsafe {
        // sie.STIE = bit 5 (Supervisor Timer Interrupt Enable).
        asm!("csrs sie, {}", in(reg) 1u64 << 5);
        // sstatus.SIE = bit 1 (Supervisor Interrupt Enable, global).
        asm!("csrs sstatus, {}", in(reg) 1u64 << 1);
    }
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
