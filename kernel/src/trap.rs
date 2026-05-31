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

/// How many ticks between timer interrupts. Set by `init_timer` from
/// the DTB timebase; the IRQ handler reads it to arm the next deadline.
pub static TIMER_INTERVAL_TICKS: AtomicU64 = AtomicU64::new(0);

/// Set by the timer IRQ handler; the main thread polls + clears.
pub static TICK_PENDING: AtomicBool = AtomicBool::new(false);

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

/// Timer IRQ handler. Kept tiny: arm the next deadline (which acks
/// the current pending bit), then set a flag so the main thread knows
/// to do the real work. **No locks taken here** — the main thread
/// owns all telemetry emission.
fn handle_timer() {
    let now: u64;
    unsafe {
        asm!("rdtime {}", out(reg) now);
    }
    let interval = TIMER_INTERVAL_TICKS.load(Ordering::Relaxed);
    arm_timer(now + interval);
    TICK_PENDING.store(true, Ordering::Relaxed);
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
    let now: u64;
    unsafe {
        asm!("rdtime {}", out(reg) now);
    }
    arm_timer(now + interval_ticks);
    unsafe { enable_timer_interrupts() };
}

/// Decoded form of the `scause` CSR. The top bit of `scause` is the
/// interrupt-vs-exception flag; the remaining bits are the cause code
/// whose meaning depends on that flag. We name the ones we handle and
/// preserve the raw code for the others.
///
/// The `u64` fields on `UnknownInterrupt` / `UnknownException` are read
/// only via the `Debug` impl in panic messages — which rustc's
/// `dead_code` lint doesn't count as a "real" use. `#[allow]` it.
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
enum TrapCause {
    SupervisorTimerInterrupt,
    SupervisorExternalInterrupt,
    SupervisorSoftwareInterrupt,
    Breakpoint,
    EnvCallFromUMode,
    EnvCallFromSMode,
    UnknownInterrupt(u64),
    UnknownException(u64),
}

fn decode_scause(scause: u64) -> TrapCause {
    let is_interrupt = (scause >> 63) & 1 == 1;
    let code = scause & !(1u64 << 63);
    if is_interrupt {
        match code {
            1 => TrapCause::SupervisorSoftwareInterrupt,
            5 => TrapCause::SupervisorTimerInterrupt,
            9 => TrapCause::SupervisorExternalInterrupt,
            other => TrapCause::UnknownInterrupt(other),
        }
    } else {
        match code {
            3 => TrapCause::Breakpoint,
            8 => TrapCause::EnvCallFromUMode,
            9 => TrapCause::EnvCallFromSMode,
            other => TrapCause::UnknownException(other),
        }
    }
}

/// Program the next supervisor timer interrupt to fire when the
/// `time` CSR reaches `deadline`. Uses the SSTC extension (the `sstc`
/// flag in the DTB ISA list confirms QEMU `virt` supports it) —
/// S-mode writes `stimecmp` directly, no SBI round-trip.
///
/// Writing a new deadline also acks any previous pending timer
/// interrupt (because `time < new_stimecmp` again until we cross it).
///
/// Some assemblers don't yet know the symbolic name `stimecmp`; we
/// use the numeric CSR address `0x14d` to be safe.
pub fn arm_timer(deadline: u64) {
    unsafe {
        asm!("csrw 0x14d, {}", in(reg) deadline);
    }
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
