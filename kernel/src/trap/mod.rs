//! S-mode trap entry, exit, and dispatch.
//!
//! `trap_entry` (defined in `trap.S`) is the symbol pointed at by
//! `stvec`. The CPU jumps here on any trap (interrupt, exception,
//! environment call). Its only job is to save the trapped GPRs, `sepc`,
//! and `sstatus` into a `TrapFrame` on the current stack, hand the frame
//! pointer to `trap_handler`, then restore everything and `sret`.
//!
//! The U-mode `ecall` surface — the syscall demux and every `handle_*`
//! handler — lives in [`crate::syscall`] (one module per call type); this
//! module keeps the trap/IRQ entry machinery (timer, CSR setup, the
//! `TrapFrame` layout, fault parking).

core::arch::global_asm!(include_str!("trap.S"));

/// The userspace runtime entry / ELF loading (`user`) and the kernel-side IPC
/// endpoint machinery (`ipc`) — both reached through the trap dispatch in this
/// module. Re-exported at the crate root so call sites stay `crate::user`,
/// `crate::ipc`.
pub mod fp;
pub mod ipc;
pub mod user;

use core::arch::asm;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use kernel_obs::clock::Clock;
use kernel_boot::trap::{FaultDisposition, TrapCause, decode_scause, exception_name};
use kernel_boot::timer::{Due, TimerWheel};

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

/// The platform timebase frequency in Hz — the rate [`now_ticks`] advances at
/// (DTB `timebase-frequency`; 10 MHz on QEMU `virt`). Stored at boot for the
/// `ClockFreq` syscall so userspace can convert ticks to real time without
/// hardcoding the platform rate. `Relaxed`: init-once at boot, read-only after.
pub static TIMEBASE_HZ: AtomicU64 = AtomicU64::new(0);

/// Timer ticks per heartbeat. The timer fires fast (so console RX is drained and
/// preemption is checked promptly — responsive interactive input), but the
/// *heartbeat* (telemetry, smoke patterns) stays at its original cadence by
/// firing only every `TICKS_PER_HEARTBEAT`-th tick. `init_timer` is given the
/// fast interval (heartbeat period ÷ this), so the heartbeat's wall-clock cadence
/// is unchanged while input latency drops by this factor. Decouples the RX drain
/// from the 1 Hz heartbeat.
pub const TICKS_PER_HEARTBEAT: u64 = 20;

/// Per-hart timer-tick counter, used to flip [`TICK_PENDING`] only once every
/// [`TICKS_PER_HEARTBEAT`] ticks (the heartbeat), while the handler drains RX and
/// checks preemption on *every* tick.
pub static TICK_COUNT: PerCpu<AtomicU64> =
    PerCpu::new([const { AtomicU64::new(0) }; crate::percpu::MAX_HARTS]);

/// Set by the timer IRQ handler once per heartbeat; the main/idle loop polls +
/// clears. One cell per hart — see block comment above.
/// `Relaxed`: same-CPU IRQ handoff — trap return sequences memory.
pub static TICK_PENDING: PerCpu<AtomicBool> =
    PerCpu::new([const { AtomicBool::new(false) }; crate::percpu::MAX_HARTS]);

/// Per-hart soonest-deadline timer wheel multiplexing this hart's one timer between
/// the scheduler tick and (when a stream plays) the audio feed. `None` until the
/// hart's first timer fire, which self-initialises it (capturing the sched interval)
/// and counts as a sched tick. Behind a `Mutex` because v2 Increment 5 enables audio
/// from another context; today only [`handle_timer`] touches it, per hart, with SIE
/// masked — no contention. With audio disabled it reduces to today's fixed cadence.
static AUDIO_WHEEL: PerCpu<crate::sync::Mutex<Option<TimerWheel>>> =
    PerCpu::new([const { crate::sync::Mutex::new(None) }; crate::percpu::MAX_HARTS]);

/// Duration of the most recent timer IRQ in ticks. The IRQ handler
/// measures `rdtime` at entry and exit; the main thread reads this
/// after wake and emits a histogram observation. One cell per hart
/// so each hart's heartbeat reports its own IRQ cost. (We can't
/// emit telemetry from the IRQ itself — would deadlock on the
/// intern / `virtio_console` mutexes.)
/// `Relaxed`: same-CPU IRQ handoff — see block comment above.
pub static LAST_IRQ_DURATION: PerCpu<AtomicU64> =
    PerCpu::new([const { AtomicU64::new(0) }; crate::percpu::MAX_HARTS]);

/// SBI-based clock: reads `time` via `rdtime`, arms via `sbi_set_timer` (SBI TIME
/// extension) rather than a direct `stimecmp` (CSR `0x14d`) write. Portable to
/// cores without Sstc — the JH7110 U74s — where a `stimecmp` write would trap;
/// QEMU and snemu service the same SBI call. Implements `kernel_obs::clock::Clock`.
pub struct SbiClock;

impl Clock for SbiClock {
    fn now(&self) -> u64 {
        let t: u64;
        // SAFETY: `rdtime` is a non-trapping read of the `time` CSR in S-mode.
        unsafe {
            asm!("rdtime {}", out(reg) t);
        }
        t
    }
    fn arm(&self, deadline: u64) {
        crate::sbi::set_timer(deadline);
    }
}

/// The clock used by the IRQ handler and boot-time timer setup. A
/// single concrete instance lives here so the handler doesn't need to
/// take a `&dyn Clock` (no allocator, and the cost of dynamic dispatch
/// in an IRQ is silly when we only ever have one impl).
pub const CLOCK: SbiClock = SbiClock;

/// The current monotonic clock tick count — the timestamp source spans use.
/// Exposed for the `ClockNow` syscall (the `Clock` trait is already in scope here).
#[must_use]
pub fn now_ticks() -> u64 {
    CLOCK.now()
}

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
        // SAFETY: `frame` points at the `TrapFrame` `trap_entry` built on this
        // hart's kernel stack; reading `sstatus` from it for the SPP gate is
        // sound and we are its sole accessor for the duration of the handler.
        TrapCause::SupervisorTimerInterrupt => handle_timer(unsafe { &*frame }),
        TrapCause::SupervisorSoftwareInterrupt => crate::ipi::handle_pending(),
        TrapCause::SupervisorExternalInterrupt => handle_external(),
        TrapCause::EnvCallFromUMode => {
            // SAFETY: `frame` points at the `TrapFrame` `trap_entry` just
            // built on this hart's kernel stack; we are its sole accessor
            // for the duration of the handler.
            crate::syscall::handle_user_ecall(unsafe { &mut *frame });
        }
        // A page fault (12/13/15) from S-mode (SPP=1) is a kernel bug. If it
        // landed in a kernel-stack guard page (Tier B), name it as a stack
        // overflow — the exact-PC fault the guard page exists to produce —
        // before panicking. Checked ahead of the general disposition below
        // because only this cause has that extra diagnosis to offer.
        TrapCause::UnknownException(12 | 13 | 15)
            if unsafe { &*frame }.sstatus & SSTATUS_SPP != 0 =>
        {
            handle_kernel_fault(scause);
        }
        // Everything with no dedicated handler: `fault_disposition` decides
        // whose bug it is. From U-mode the faulting instruction was the
        // process's, so the process dies and the machine carries on — a page
        // fault caught by the `U`-bit firewall, an illegal instruction (what an
        // FP instruction is while `sstatus.FS` is Off), or a code we've never
        // seen. From S-mode, or for an interrupt from a source we never enabled,
        // there's nothing smaller to terminate: panic.
        other => {
            let from_user = unsafe { &*frame }.sstatus & SSTATUS_SPP == 0;
            // An illegal instruction from U-mode may be a *request* for floating
            // point rather than a fault: `sstatus.FS` starts Off, so a program's
            // first FP instruction traps here. If the process is authorised, FP is
            // enabled and the instruction retried — `sepc` is deliberately NOT
            // advanced, so the `sret` re-executes it, now legally. Anything else
            // (unauthorised, or FS already on so the instruction really is illegal)
            // falls through to the terminate path below, having snitched its reason.
            //
            // SAFETY: as the other `frame` uses in this handler — sole accessor for
            // the duration, and `try_enable` mutates only the saved `sstatus`.
            let fp_enabled = from_user
                && matches!(other, TrapCause::UnknownException(EXC_ILLEGAL_INSTRUCTION))
                && fp::try_enable(unsafe { &mut *frame });
            // Not an early `return`: the kill checkpoint at the end of this handler
            // must still run on the way back to U-mode.
            if !fp_enabled {
                match kernel_boot::trap::fault_disposition(other, from_user) {
                    FaultDisposition::TerminateProcess => {
                        terminate_faulting_process(scause & !(1u64 << 63));
                    }
                    FaultDisposition::KernelPanic => {
                        panic!("unhandled trap: {other:?} (scause={scause:#x})")
                    }
                }
            }
        }
    }

    // v2b cross-hart Kill checkpoint. Only on a return to **U-mode** (SPP == 0) — never
    // mid-kernel-work, which could hold a lock or leave state half-updated. The cheap
    // per-hart gate means the scheduler lock is taken only when a kill is actually
    // pending (armed by the `IPI_KILL_CHECK` handler or the scheduler running a flagged
    // task), not on every trap. `exit_if_kill_requested` never returns if it fires.
    let from_user = unsafe { &*frame }.sstatus & SSTATUS_SPP == 0;
    if from_user
        && crate::percpu::this_cpu()
            .pending_kill_check
            .swap(false, core::sync::atomic::Ordering::Acquire)
    {
        crate::sched::exit_if_kill_requested();
    }
}

/// An S-mode (kernel) page fault. If `stval` lands in a kernel-stack guard page,
/// report it as a named stack overflow ([`crate::sched::report_stack_guard_fault`]);
/// otherwise it's an ordinary kernel bug — panic with the cause + address.
///
/// Caveat: the handler runs on the *faulting* kernel stack (in-kernel traps reuse
/// the current stack — `sscratch == 0`), so a *deep* overflow that creeps to the
/// page boundary can double-fault while building the trap frame. Robust reporting
/// of deep overflows needs a per-hart exception stack (the Linux double-fault
/// stack) — documented follow-up in `plans/legacy/kernel-stack-guard-pages.md`. The guard
/// page still converts silent corruption into a deterministic fault either way.
fn handle_kernel_fault(scause: u64) -> ! {
    // The boot-stack (task 0) guard page symbol, from the linker script — a single
    // page below the boot stack, in the kernel image rather than the window (see
    // `mmu::guard_boot_stack`). Declared up front; used below.
    unsafe extern "C" {
        static __boot_stack_guard: u8;
    }
    let stval: usize;
    // SAFETY: reads a CSR; no memory access, no side effects.
    unsafe { asm!("csrr {}, stval", out(reg) stval, options(nomem, nostack)) };
    // Per-task kernel-stack window guard.
    if let Some(slot) = kernel_proc::stack::guard_slot_for(stval) {
        crate::sched::report_stack_guard_fault(slot, stval);
    }
    let boot_guard = (&raw const __boot_stack_guard) as usize;
    if (boot_guard..boot_guard + kernel_mem::mmu::PAGE_SIZE).contains(&stval) {
        crate::sched::report_boot_stack_guard_fault(stval);
    }
    // `sepc` is the faulting PC — the single most useful field for locating a
    // kernel fault (look it up with `rust-nm`/`addr2line` against the kernel ELF).
    let sepc: usize;
    // SAFETY: reads a CSR; no memory access, no side effects.
    unsafe { asm!("csrr {}, sepc", out(reg) sepc, options(nomem, nostack)) };
    panic!("kernel page fault: scause={scause:#x} stval={stval:#x} sepc={sepc:#x}");
}

/// `sstatus.SPP` (bit 8): the privilege the trap came from. 0 = User.
const SSTATUS_SPP: u64 = 1 << 8;

/// `scause` code for an illegal instruction — what an FP instruction produces while
/// `sstatus.FS` is Off, and so the trap the lazy FP enable hangs off.
const EXC_ILLEGAL_INSTRUCTION: u64 = 2;

/// A U-mode trap the kernel has no handler for — the faulting instruction was
/// the process's, so the **process** dies and the machine carries on. Returning
/// would re-run the faulting instruction forever, so this never returns.
///
/// `code` is the `scause` exception code (interrupt bit already cleared). Snitch
/// first — `snitchos.user.faults_total` plus a `Log` naming the cause, `sepc` and
/// `stval`, so the fault is attributable rather than a process that merely
/// vanished — then terminate exactly as the cross-hart kill checkpoint does:
/// record the zombie with [`crate::sched::FAULTED_STATUS`], wake any parent
/// blocked in `Wait`, and hand the hart to its next ready task.
///
/// This replaces v0.7a's `loop { wfi }` park, which predated process teardown.
/// Parking leaked a whole hart per fault, and a fault on hart 0 took the
/// heartbeat with it — so the isolation firewall's success looked, from the
/// wire, exactly like the kernel dying.
fn terminate_faulting_process(code: u64) -> ! {
    if let Some(id) = crate::user::user_fault_metric_id() {
        crate::tracing::emit_metric(id, 1);
    }

    let (sepc, stval): (usize, usize);
    // SAFETY: two CSR reads; no memory access, no side effects.
    unsafe {
        asm!("csrr {}, sepc", out(reg) sepc, options(nomem, nostack));
        asm!("csrr {}, stval", out(reg) stval, options(nomem, nostack));
    }

    let me = crate::sched::current_task_id();
    crate::tracing::emit_log(&alloc::format!(
        "user fault: task {} killed by {} (scause={code} sepc={sepc:#x} stval={stval:#x})",
        me.0,
        exception_name(code),
    ));

    // A faulting process can be holding FP just as an exiting one can, so release the
    // claim on this path too — otherwise a process killed mid-FP takes FP with it.
    crate::syscall::process::release_fp_claim();

    // The process is gone; this hart is no longer running it. Mirrors
    // `handle_exit` — the pointer must not outlive the address space.
    crate::process::CURRENT_PROCESS
        .this_cpu()
        .store(core::ptr::null_mut(), Ordering::Relaxed);

    // Record the zombie + wake any parent blocked in `Wait` on us, then exit via
    // the *owned* path so the status survives for the parent to reap. A
    // parentless faulted task leaves a zombie until reaped — same as `Exit`.
    if let Some(parent) = crate::sched::note_exit(me, crate::sched::FAULTED_STATUS) {
        crate::sched::wake(parent);
    }
    crate::sched::exit_now_owned()
}

/// Start feeding audio on **this hart's** timer wheel: enable the audio deadline at
/// `period_ticks` and re-arm the timer to the (now sooner) deadline so the DAC drain
/// begins promptly. Called from the `AudioEnqueue` path when a stream begins. The DAC
/// MMIO is global, so whichever hart enqueues is the one that drains — no cross-hart
/// hand-off. Re-arming resets the audio deadline, so the caller must invoke this only
/// on the idle→active transition (the pwmdac `AUDIO_FEEDING` latch guarantees it).
pub fn enable_audio_feed(period_ticks: u64) {
    let now = CLOCK.now();
    let interval = TIMER_INTERVAL_TICKS.load(Ordering::Relaxed);
    let mut wheel = AUDIO_WHEEL.this_cpu().lock();
    wheel
        .get_or_insert_with(|| TimerWheel::new(now, interval))
        .enable_audio(now, period_ticks);
    let deadline = wheel.as_ref().map_or(now + period_ticks, TimerWheel::deadline);
    drop(wheel);
    CLOCK.arm(deadline);
}

/// Stop feeding audio on **this hart's** timer wheel — the audio deadline drops out of
/// the multiplex and the timer reverts to the scheduler cadence on its next fire.
/// Called from the drain when the ring goes idle (v2 Increment 5).
pub fn disable_audio_feed() {
    if let Some(wheel) = AUDIO_WHEEL.this_cpu().lock().as_mut() {
        wheel.disable_audio();
    }
}

/// Timer IRQ handler. Kept tiny: measure duration, arm the next
/// deadline (which acks the current pending bit), then set a flag so
/// the main thread knows to do the real work. **No locks taken here**
/// — the main thread owns all telemetry emission.
fn handle_timer(frame: &TrapFrame) {
    let start = CLOCK.now();
    let interval = TIMER_INTERVAL_TICKS.load(Ordering::Relaxed);

    // Multiplex this hart's single timer between the scheduler tick and the audio
    // feed via the soonest-deadline wheel. The wheel self-initialises on the first
    // fire — which IS a sched tick — capturing the interval; thereafter `poll`
    // decides which deadline(s) are due. Arm the next deadline (acking the current
    // pending bit) before doing any work. With audio disabled the wheel yields the
    // same fixed cadence as before, so nothing below changes for the scheduler.
    let (due, deadline) = {
        let mut wheel = AUDIO_WHEEL.this_cpu().lock();
        let due = match wheel.as_mut() {
            None => {
                *wheel = Some(TimerWheel::new(start, interval));
                Due { audio: false, sched: true }
            }
            Some(w) => w.poll(start),
        };
        let deadline = wheel.as_ref().map_or(start + interval, TimerWheel::deadline);
        (due, deadline)
    };
    CLOCK.arm(deadline);

    // Feed one DAC sample per audio deadline (dormant until v2 Increment 5 enables
    // audio; the drain is a leaf lock like CONSOLE_RX, safe in the IRQ).
    if due.audio {
        crate::pwmdac::drain_one();
    }

    if due.sched {
        // The timer fires fast for responsive RX drain + preemption, but the
        // heartbeat runs only every `TICKS_PER_HEARTBEAT`-th tick (its wall-clock
        // cadence unchanged) — so the typing-latency win doesn't flood telemetry.
        let ticks = TICK_COUNT.this_cpu().fetch_add(1, Ordering::Relaxed) + 1;
        if ticks.is_multiple_of(TICKS_PER_HEARTBEAT) {
            TICK_PENDING.this_cpu().store(true, Ordering::Relaxed);
        }

        // Drain the UART RX FIFO into the console ring — hart 0 only (single
        // producer). Locking CONSOLE_RX here is the *exception* to "no locks in the
        // timer handler": it's a leaf lock taken only by this drain and ConsoleRead,
        // both run with SIE==0 (can't nest on one hart), and neither allocates nor
        // emits — unlike the virtio/println locks. Must precede maybe_preempt, which
        // may switch away and not return on this pass.
        if crate::percpu::current_hartid() == 0 {
            crate::console::drain_rx();
        }

        // v2b timed waits: wake any task on this hart whose timeout deadline has
        // passed, so its wait loop re-checks and returns `TimedOut`. Before
        // `maybe_preempt`, which may switch away and not return this pass.
        crate::sched::wake_expired_timeouts(start);

        // v0.8 preemption: if this timer interrupted a *userspace* task that has
        // overrun its quantum, deschedule it now. `SPP == 0` means the trap came
        // from U-mode; kernel code (`SPP == 1`) is never preempted, keeping the
        // cooperative "exclusive until I yield" invariant. When the descheduled
        // task is next picked, it resumes here, returns, and `trap_entry` restores
        // its full `TrapFrame` and `sret`s to the exact user PC it was running.
        crate::sched::maybe_preempt(frame.sstatus & SSTATUS_SPP == 0);
    }

    let end = CLOCK.now();
    LAST_IRQ_DURATION
        .this_cpu()
        .store(end.wrapping_sub(start), Ordering::Relaxed);
}

/// Handle a supervisor external (PLIC) interrupt: claim the top pending source,
/// dispatch on it, and complete it so the source re-arms.
///
/// Today the only routed source is the UART; the drain of its TX ring lands in a
/// later increment, so a claimed UART interrupt is currently just acknowledged.
/// Nothing asserts until the UART's THRE interrupt is enabled, so at runtime this
/// does not yet run — it is the wired-but-inert half of the interrupt path.
fn handle_external() {
    while let Some(source) = crate::plic::claim() {
        if crate::plic::is_uart(source) {
            crate::console::drain_tx();
        }
        crate::plic::complete(source);
    }
}

/// Run `f` with S-mode interrupts masked (`sstatus.SIE` cleared), restoring the
/// prior state after. The minimal critical section for code that enables a device
/// interrupt while holding a lock the interrupt handler also takes — without this,
/// the interrupt could fire mid-section and the handler would deadlock re-taking
/// the lock. (`kernel::sync::Mutex` reserves hooks for this but they're still
/// no-ops; this is the targeted primitive until IRQ-safe locking lands.)
pub fn without_interrupts<T>(f: impl FnOnce() -> T) -> T {
    let sstatus: u64;
    // SAFETY: `csrrci` reads sstatus and clears bit 1 (SIE) atomically; no memory
    // touched, only the named output.
    unsafe { asm!("csrrci {}, sstatus, 0b10", out(reg) sstatus, options(nomem, nostack)) };
    let result = f();
    if sstatus & (1 << 1) != 0 {
        // SAFETY: restore SIE only if it was set on entry.
        unsafe { asm!("csrsi sstatus, 0b10", options(nomem, nostack)) };
    }
    result
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

/// Enable S-mode external (PLIC) interrupts. `sie.SEIE` = bit 9. Inert until a
/// PLIC source actually asserts (the UART's THRE interrupt is enabled in a later
/// increment), since `handle_external` only runs when the PLIC signals.
///
/// # Safety
///
/// Trap vector must be installed and the PLIC routed (`plic::init`). Any already-
/// asserting source fires immediately on return.
pub unsafe fn enable_external_interrupts() {
    unsafe {
        // sie.SEIE = bit 9.
        asm!("csrs sie, {}", in(reg) 1u64 << 9);
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
