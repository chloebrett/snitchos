//! Lazy hardware-FP enable: the kernel side of the authority decision.
//!
//! The kernel itself stays **zero-FP** — it never executes a floating-point
//! instruction, by design (same call as the fixed-point audio path). This module is
//! only about what *userspace* may do.
//!
//! The mechanism is classic lazy-FP context switching used as an authority check.
//! `sstatus.FS` starts `Off`, so a user program's first FP instruction traps as
//! illegal; that trap is the enable-or-refuse decision point. An integer-only program
//! never traps and so pays nothing — which is where the cost attribution actually
//! comes from, *not* from the ELF flag (every binary in this tree is built `lp64d` and
//! so claims hard float; see [`kernel_proc::elf::FloatAbi`]).
//!
//! Policy is pure and host-tested in [`kernel_proc::fp`]; this file supplies the three
//! facts, mutates the trap frame, and snitches.

use core::sync::atomic::{AtomicUsize, Ordering};

use kernel_proc::fp::{FpEnableDecision, enable_decision};
use protocol::StringId;

use crate::sync::Once;
use crate::trap::TrapFrame;
use crate::tracing;

/// `sstatus.FS`, bits 14:13 — the FP unit's state (0 = Off, 1 = Initial, 2 = Clean,
/// 3 = Dirty).
const SSTATUS_FS: u64 = 0b11 << 13;
/// `FS = Initial`: FP on, registers at their reset values.
const SSTATUS_FS_INITIAL: u64 = 0b01 << 13;

/// How many processes currently hold live FP register state.
///
/// Exists to enforce the interim one-FP-process-at-a-time guard: the kernel cannot yet
/// save FP registers across a context switch, so a second FP process would silently
/// share the first's 32 registers. Counting lets that become an observable refusal
/// instead. Goes away with FP context switching.
///
/// `Relaxed`: a counter read and bumped inside the trap handler of whichever hart
/// faulted; there is no payload published through it.
static FP_HOLDERS: AtomicUsize = AtomicUsize::new(0);

/// `snitchos.fp.processes_enabled` — how many processes have been granted FP. The
/// point of the metric is cost attribution: this is exactly the set of processes whose
/// context switches will have to carry 256 bytes of FP registers.
static FP_ENABLED_METRIC: Once<StringId> = Once::new();
/// `snitchos.fp.refused_total` — FP requests refused, whether for lack of authority or
/// because another process holds the registers. Refusals snitch, never silent.
static FP_REFUSED_METRIC: Once<StringId> = Once::new();

/// Register this module's counters. Call once at boot, before U-mode is entered, so
/// the trap handler never has to intern a string in trap context.
pub fn init_metrics() {
    FP_ENABLED_METRIC.call_once(|| tracing::register_counter("snitchos.fp.processes_enabled"));
    FP_REFUSED_METRIC.call_once(|| tracing::register_counter("snitchos.fp.refused_total"));
}

/// Handle a U-mode illegal-instruction trap as a possible request for floating point.
///
/// Returns `true` if FP was enabled and the caller should **return to the faulting
/// instruction without advancing `sepc`**, so the instruction re-executes — now
/// legally. Returns `false` if this wasn't an FP-enable situation or was refused, in
/// which case the caller's ordinary "unhandled user trap kills the process" path
/// takes over (a refusal is snitched here first, so the reason is on the wire before
/// the process dies).
pub fn try_enable(frame: &mut TrapFrame) -> bool {
    // No current process (a kernel task hit this) → nothing to authorise.
    let process = crate::process::CURRENT_PROCESS.this_cpu().load(Ordering::Relaxed);
    if process.is_null() {
        return false;
    }
    // SAFETY: `CURRENT_PROCESS` points at the `Process` in the frame of the
    // never-returning `enter` for the task running on this hart; it stays valid for
    // every trap from that task's U-mode.
    let process = unsafe { &*process };

    let authorised = process.fp_authorised.load(Ordering::Relaxed);
    let fs_off = frame.sstatus & SSTATUS_FS == 0;
    // This process isn't a holder: if it were, its saved `FS` would not be Off.
    let other_holders = FP_HOLDERS.load(Ordering::Relaxed);

    match enable_decision(authorised, fs_off, other_holders) {
        FpEnableDecision::Enable => {
            // Turn FP on in the *saved* `sstatus`, so the `sret` that returns to the
            // faulting instruction restores it with FP live. `Initial` rather than
            // `Clean`: the registers hold nothing meaningful yet, and the first FP
            // write promotes it to `Dirty`.
            frame.sstatus = (frame.sstatus & !SSTATUS_FS) | SSTATUS_FS_INITIAL;
            process.fp_enabled.store(true, Ordering::Relaxed);
            FP_HOLDERS.fetch_add(1, Ordering::Relaxed);
            if let Some(id) = FP_ENABLED_METRIC.get().copied() {
                tracing::emit_metric(id, 1);
            }
            // Which process, and on what authority — an authority grant should be as
            // observable as a capability grant.
            tracing::emit_log(&alloc::format!(
                "fp enabled: task {} (authority: ELF hard-float ABI)",
                crate::sched::current_task_id().0,
            ));
            true
        }
        FpEnableDecision::RefuseUnauthorised => {
            refuse("program declares the soft-float ABI");
            false
        }
        FpEnableDecision::RefuseBusy => {
            refuse(
                "another process holds the FP registers and the kernel cannot yet save \
                 them across a context switch",
            );
            false
        }
        FpEnableDecision::NotFpRelated => false,
    }
}

/// Snitch a refused FP request before the caller kills the process. Structured enough
/// to answer "which process, and why" from the wire alone.
fn refuse(reason: &str) {
    if let Some(id) = FP_REFUSED_METRIC.get().copied() {
        tracing::emit_metric(id, 1);
    }
    tracing::emit_log(&alloc::format!(
        "fp refused: task {} — {reason}",
        crate::sched::current_task_id().0,
    ));
}

/// Release this process's claim on the FP registers, called when a process that had FP
/// enabled exits. Without it the interim one-at-a-time guard would leak: the first FP
/// process's death would permanently deny FP to everyone after it.
pub fn release(had_fp: bool) {
    if had_fp {
        FP_HOLDERS.fetch_sub(1, Ordering::Relaxed);
    }
}
