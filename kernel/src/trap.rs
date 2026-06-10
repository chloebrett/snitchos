//! S-mode trap entry, exit, and dispatch.
//!
//! `trap_entry` (defined in `trap.S`) is the symbol pointed at by
//! `stvec`. The CPU jumps here on any trap (interrupt, exception,
//! environment call). Its only job is to save the trapped GPRs, `sepc`,
//! and `sstatus` into a `TrapFrame` on the current stack, hand the frame
//! pointer to `trap_handler`, then restore everything and `sret`.

core::arch::global_asm!(include_str!("trap.S"));

use core::arch::asm;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use kernel_core::clock::Clock;
use kernel_core::trap::{TrapCause, decode_scause};

use crate::percpu::PerCpu;

// ## Memory ordering note for the timer-IRQ statics below
//
// `TICK_PENDING` (set by ISR, read by main) and `LAST_IRQ_DURATION`
// (written by ISR, read by main after observing TICK_PENDING) form a
// classic publication pattern. Across harts that pattern needs
// `Release` on the store side and `Acquire` on the load side.
//
// Both are now `PerCpu<T>`: each hart's ISR touches only its own
// cell, and that hart's main/idle loop reads the same cell. Both
// ends are guaranteed same-hart by construction (the ISR runs on
// whichever hart's `stimecmp` expired; `this_cpu()` reads `tp`).
// Trap return synchronises the handler's memory ops with the
// resumed thread by hardware, so `Relaxed` is correct.
//
// Pre-PerCpu these were globals shared by both harts. Hart 0's ISR
// could clobber a tick that hart 1 had not yet polled (correctness
// for the heartbeat cadence on the secondary) and hart 0's
// heartbeat could observe hart 1's `LAST_IRQ_DURATION` (telemetry
// corruption). See `plans/deflake-bisection.md` follow-up (c).

/// How many ticks between timer interrupts. Set by `init_timer` from
/// the DTB timebase; both harts' IRQ handlers read it to arm the
/// next deadline. Init-once global shared config — the cadence is
/// the same on every hart, so there's no per-CPU state to track.
/// `Relaxed`: init-once, then read forever — no payload to publish.
pub static TIMER_INTERVAL_TICKS: AtomicU64 = AtomicU64::new(0);

/// Set by the timer IRQ handler; the main/idle loop polls + clears.
/// One cell per hart — see block comment above.
/// `Relaxed`: same-CPU IRQ handoff — trap return sequences memory.
pub static TICK_PENDING: PerCpu<AtomicBool> =
    PerCpu::new([AtomicBool::new(false), AtomicBool::new(false)]);

/// Duration of the most recent timer IRQ in ticks. The IRQ handler
/// measures `rdtime` at entry and exit; the main thread reads this
/// after wake and emits a histogram observation. One cell per hart
/// so each hart's heartbeat reports its own IRQ cost. (We can't
/// emit telemetry from the IRQ itself — would deadlock on the
/// intern / virtio_console mutexes.)
/// `Relaxed`: same-CPU IRQ handoff — see block comment above.
pub static LAST_IRQ_DURATION: PerCpu<AtomicU64> =
    PerCpu::new([AtomicU64::new(0), AtomicU64::new(0)]);

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
pub extern "C" fn trap_handler(frame: *mut TrapFrame) {
    let scause: u64;
    unsafe {
        asm!("csrr {}, scause", out(reg) scause);
    }
    match decode_scause(scause) {
        TrapCause::SupervisorTimerInterrupt => handle_timer(),
        TrapCause::SupervisorSoftwareInterrupt => crate::ipi::handle_pending(),
        TrapCause::EnvCallFromUMode => {
            // SAFETY: `frame` points at the `TrapFrame` `trap_entry` just
            // built on this hart's kernel stack; we are its sole accessor
            // for the duration of the handler.
            handle_user_ecall(unsafe { &mut *frame });
        }
        // Instruction/load/store page fault (codes 12/13/15) from U-mode is
        // the isolation firewall catching userspace touching memory it has no
        // `U`-bit access to. Count it and park (v0.7a has no process teardown).
        // The same fault from S-mode is a real kernel bug — fall through to panic.
        TrapCause::UnknownException(12 | 13 | 15)
            if unsafe { &*frame }.sstatus & SSTATUS_SPP == 0 =>
        {
            handle_user_fault();
        }
        other => panic!("unhandled trap: {other:?} (scause={scause:#x})"),
    }
}

/// `sstatus.SPP` (bit 8): the privilege the trap came from. 0 = User.
const SSTATUS_SPP: u64 = 1 << 8;

/// A U-mode access faulted — the page-table `U`-bit firewall did its job
/// (v0.7a has no capability layer yet; that's v0.7b). Count it and park this
/// hart: with no process teardown we can't reschedule, and returning would
/// re-run the faulting instruction forever. Hart 0 carries on. Never returns.
fn handle_user_fault() -> ! {
    if let Some(id) = crate::user::user_fault_metric_id() {
        crate::tracing::emit_metric(id, 1);
    }
    loop {
        // SAFETY: park until the next interrupt; nothing to do on this hart.
        unsafe { asm!("wfi", options(nomem, nostack)) };
    }
}

/// Handle an `ecall` from U-mode. The v0.7b kernel surface is **invoke a
/// capability**: `a7` selects the syscall (`Invoke`), `a0` is the handle
/// into the *calling process's* `CapTable`, `a1` the argument. We resolve
/// and rights-check against that table (no ambient authority), then advance
/// `sepc` past the `ecall`.
fn handle_user_ecall(frame: &mut TrapFrame) {
    use snitchos_abi::Syscall;
    match Syscall::from_usize(frame.a7 as usize) {
        Some(Syscall::Invoke) => handle_invoke(frame),
        None => frame.a0 = u64::MAX, // unknown syscall
    }
    // `ecall` is a 4-byte instruction; without advancing past it, `sret`
    // would re-execute it and we'd trap on it forever.
    frame.sepc = frame.sepc.wrapping_add(4);
}

/// Capability invocation. Resolve `a0` against the running process's
/// `CapTable`; on success perform the authorized operation (emit `a1` to
/// the `TelemetrySink`'s bound counter), else refuse with a nonzero `a0`.
/// The authority decision itself is the pure, host-tested
/// [`kernel_core::cap::invoke_telemetry`]; here we only act on its result.
fn handle_invoke(frame: &mut TrapFrame) {
    use kernel_core::cap::{Handle, invoke_telemetry};

    let proc = crate::process::CURRENT_PROCESS.this_cpu().load(Ordering::Relaxed);
    // SAFETY: set by `user::run` on this hart before `sret`; the `Process`
    // lives in that never-returning frame. Null only if no user process is
    // running here — which then could not have issued this U-mode `ecall`.
    let Some(proc) = (unsafe { proc.as_ref() }) else {
        frame.a0 = u64::MAX;
        return;
    };

    let handle = Handle::from_raw(frame.a0 as u32);
    // Resolve under the lock, copy out the counter, drop the lock before
    // emitting — never hold a Mutex across telemetry emission.
    let outcome = invoke_telemetry(&proc.caps.lock(), handle);
    match outcome {
        Ok(counter) => {
            crate::tracing::emit_metric(counter, frame.a1 as i64);
            frame.a0 = 0; // success
        }
        Err(_denied) => {
            // Snitch the refused authority decision. Counter is pre-
            // registered (`user::init_metric`), so no interning in trap
            // context. The richer `CapEvent` frame is the sequenced
            // follow-on (carries granter/object/rights a counter can't).
            if let Some(id) = crate::user::cap_denied_metric_id() {
                crate::tracing::emit_metric(id, 1);
            }
            frame.a0 = u64::MAX; // refused
        }
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
    TICK_PENDING.this_cpu().store(true, Ordering::Relaxed);
    let end = CLOCK.now();
    LAST_IRQ_DURATION
        .this_cpu()
        .store(end.wrapping_sub(start), Ordering::Relaxed);
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

/// Enable S-mode software interrupts (IPIs). `sie.SSIE` = bit 1.
/// `sstatus.SIE` is set globally by `enable_timer_interrupts`;
/// call this either before or after — the per-source bit is what
/// gates SSIP-driven trap entry.
///
/// # Safety
///
/// Trap vector must be installed and `ipi::handle_pending` must be
/// ready to run. Any pending `SSIP` from before this call fires
/// immediately on return.
pub unsafe fn enable_software_interrupts() {
    unsafe {
        // sie.SSIE = bit 1.
        asm!("csrs sie, {}", in(reg) 1u64 << 1);
    }
}

/// Install our `trap_entry` (from `trap.S`) as the S-mode trap vector,
/// and establish the in-kernel `sscratch` convention.
/// After this returns, every trap (exception or interrupt) routes to
/// our handler. Call once per hart, at boot, before anything that might
/// trap.
///
/// `sscratch` is zeroed here: `trap_entry`'s stack-switch swap uses
/// `sscratch == 0` as the "we were already in the kernel, this is a
/// trusted stack" sentinel. While running user code the scheduler parks
/// the thread's kernel stack top in `sscratch` instead; the trap exit
/// re-arms it. At boot we are in the kernel, so the sentinel is 0.
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
          "csrw stvec, {}",
          "csrw sscratch, zero",
          in(reg) addr,
        );
    }
}
