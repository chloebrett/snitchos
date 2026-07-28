//! Carrying the FP register file across a context switch.
//!
//! **Why this is Rust and not `sched.S`.** A task's FP state is only ever endangered
//! by *another task running FP*, and the switch is the one place that happens — but
//! the copy itself needs no knowledge of the switch's register discipline, so it does
//! not have to live inside the asm that implements it. Keeping it here means
//! `sched.S` stays the small, auditable thing it was, and the policy
//! ([`kernel_proc::fp::switch_action`]) stays host-tested.
//!
//! **The symmetry that makes this work.** `switch(from, to)` returns *in the task
//! that called it*, on some later switch back. So "save the outgoing task" and
//! "restore the incoming task" are the same task's before-and-after:
//!
//! ```text
//! save_before(me);       // my registers, while they are still mine
//! switch(me, next);      // ... other tasks run, and use the FP unit ...
//! restore_after(me);     // I am back; put mine back
//! ```
//!
//! Each task restores its own state on the way in, so a task that has never been
//! switched out (a freshly spawned one, resuming at its entry point rather than
//! inside `switch`) correctly restores nothing — it owns nothing yet.

use core::arch::asm;
use core::sync::atomic::Ordering;

use kernel_proc::fp::{FpState, switch_action};

use super::TaskContext;

/// `sstatus.FS`, bits 14:13.
const SSTATUS_FS_SHIFT: u64 = 13;
const SSTATUS_FS: u64 = 0b11 << SSTATUS_FS_SHIFT;

/// Switches that copied a task's FP registers out, and switches that copied a task's
/// back in. Deferred counters, not frames: this is the context-switch path, and
/// emitting from it would take the virtio lock (and, on a first use, the intern
/// table's allocator) inside the one operation that must not re-enter. The heartbeat
/// drains them, like every other hot-path counter.
///
/// Worth having on the wire rather than merely working: the whole case for lazy FP is
/// that programs which never touch it never pay, and these two counters are what makes
/// that claim checkable instead of asserted — they stay flat while only integer tasks
/// run, and name the cost the moment an FP task is scheduled.
pub static FP_SAVES: crate::obs::counter::DeferredCounter =
    crate::obs::counter::DeferredCounter::new("snitchos.fp.context_saves_total");
pub static FP_RESTORES: crate::obs::counter::DeferredCounter =
    crate::obs::counter::DeferredCounter::new("snitchos.fp.context_restores_total");

/// Put this hart's FP unit into the state the rest of the design assumes: **off**.
///
/// Not a formality. `sstatus.FS` arrives from firmware, and firmware does not
/// guarantee `Off` — measured 2026-07-28, QEMU's OpenSBI hands S-mode a hart whose
/// `FS` is already set, while snemu resets it to `Off`. Two things depend on the
/// difference:
///
/// - **The authority check only fires while `FS` is `Off`.** A user task inherits
///   `FS` from the kernel context that enters it, so a hart that never cleared it
///   would enter U-mode with floating point already enabled — no trap, no
///   [`kernel_proc::fp::enable_decision`], no `Log` naming the grant. FP would be
///   ambient authority, silently.
/// - **The first switch would charge someone else's dirt.** `FS = Dirty` out of
///   firmware means the first task to be switched out gets its registers saved as
///   though it had written them.
///
/// Cheap insurance either way: one CSR write per hart at boot, and it makes the two
/// engines agree about a fact neither of them should be deciding.
pub fn init_hart() {
    write_fs(FpState::Off);
}

/// Read the live `sstatus.FS`.
///
/// The *live* CSR, not the trap frame's copy: it is what the outgoing task last left
/// the FP unit in. Hardware sets `Dirty` on any FP register write, including one made
/// in U-mode before the trap that brought us here — trap entry does not disturb `FS`,
/// and the kernel executes no FP of its own, so nothing between there and here can
/// have changed it.
fn read_fs() -> FpState {
    let sstatus: u64;
    // SAFETY: a CSR read with no side effects.
    unsafe { asm!("csrr {}, sstatus", out(reg) sstatus, options(nomem, nostack)) };
    match (sstatus & SSTATUS_FS) >> SSTATUS_FS_SHIFT {
        0 => FpState::Off,
        1 => FpState::Initial,
        2 => FpState::Clean,
        _ => FpState::Dirty,
    }
}

/// Set the live `sstatus.FS`.
fn write_fs(state: FpState) {
    let bits = (state as u64) << SSTATUS_FS_SHIFT;
    // SAFETY: a read-modify-write of `sstatus.FS` only. `FS` governs whether FP
    // instructions trap; every other field is preserved by the mask.
    unsafe {
        asm!(
            "csrc sstatus, {mask}",
            "csrs sstatus, {bits}",
            mask = in(reg) SSTATUS_FS,
            bits = in(reg) bits,
            options(nomem, nostack),
        );
    }
}

/// Copy `f0`–`f31` + `fcsr` out of the FP unit into `dst`.
///
/// # Safety
///
/// `dst` must point at a live [`super::FpRegs`]. `FS` must not be `Off` — an `fsd`
/// with the unit off would itself trap as illegal.
unsafe fn save_registers(dst: *mut u64) {
    // SAFETY: caller guarantees `dst` addresses 32 `u64`s followed by `fcsr`, and
    // that the FP unit is on. Reads registers only; writes only through `dst`.
    unsafe {
        asm!(
            "fsd f0,    0({d})", "fsd f1,    8({d})", "fsd f2,   16({d})", "fsd f3,   24({d})",
            "fsd f4,   32({d})", "fsd f5,   40({d})", "fsd f6,   48({d})", "fsd f7,   56({d})",
            "fsd f8,   64({d})", "fsd f9,   72({d})", "fsd f10,  80({d})", "fsd f11,  88({d})",
            "fsd f12,  96({d})", "fsd f13, 104({d})", "fsd f14, 112({d})", "fsd f15, 120({d})",
            "fsd f16, 128({d})", "fsd f17, 136({d})", "fsd f18, 144({d})", "fsd f19, 152({d})",
            "fsd f20, 160({d})", "fsd f21, 168({d})", "fsd f22, 176({d})", "fsd f23, 184({d})",
            "fsd f24, 192({d})", "fsd f25, 200({d})", "fsd f26, 208({d})", "fsd f27, 216({d})",
            "fsd f28, 224({d})", "fsd f29, 232({d})", "fsd f30, 240({d})", "fsd f31, 248({d})",
            "frcsr {c}",
            "sd {c}, 256({d})",
            d = in(reg) dst,
            c = out(reg) _,
            options(nostack),
        );
    }
}

/// Load `f0`–`f31` + `fcsr` from `src` back into the FP unit.
///
/// # Safety
///
/// `src` must point at a live [`super::FpRegs`]. `FS` must not be `Off`.
unsafe fn restore_registers(src: *const u64) {
    // SAFETY: caller guarantees `src` addresses 32 `u64`s followed by `fcsr`, and
    // that the FP unit is on. Writes FP registers only; reads only through `src`.
    unsafe {
        asm!(
            "fld f0,    0({s})", "fld f1,    8({s})", "fld f2,   16({s})", "fld f3,   24({s})",
            "fld f4,   32({s})", "fld f5,   40({s})", "fld f6,   48({s})", "fld f7,   56({s})",
            "fld f8,   64({s})", "fld f9,   72({s})", "fld f10,  80({s})", "fld f11,  88({s})",
            "fld f12,  96({s})", "fld f13, 104({s})", "fld f14, 112({s})", "fld f15, 120({s})",
            "fld f16, 128({s})", "fld f17, 136({s})", "fld f18, 144({s})", "fld f19, 152({s})",
            "fld f20, 160({s})", "fld f21, 168({s})", "fld f22, 176({s})", "fld f23, 184({s})",
            "fld f24, 192({s})", "fld f25, 200({s})", "fld f26, 208({s})", "fld f27, 216({s})",
            "fld f28, 224({s})", "fld f29, 232({s})", "fld f30, 240({s})", "fld f31, 248({s})",
            "ld {c}, 256({s})",
            "fscsr {c}",
            s = in(reg) src,
            c = out(reg) _,
            options(nostack),
        );
    }
}

/// Does the task now running on this hart hold FP state?
///
/// Read off the *process*, which is where the authority lives — a kernel task has no
/// process and so never owns FP, which is also why the kernel can stay zero-FP
/// without any of this costing it anything.
fn current_owns_fp() -> bool {
    let process = crate::process::CURRENT_PROCESS.this_cpu().load(Ordering::Relaxed);
    if process.is_null() {
        return false;
    }
    // SAFETY: the same `CURRENT_PROCESS` deref the syscall and trap paths make — it
    // points at the `Process` living in the never-returning `enter` frame of this
    // hart's current task.
    unsafe { &*process }.fp_enabled.load(Ordering::Relaxed)
}

/// Save this task's FP registers, if it has any worth saving, and hand the FP unit
/// over in a neutral state.
///
/// Leaving `FS = Off` is not tidiness: it is what makes the *next* task's decision
/// honest. `FS` is a per-hart CSR, so a stale `Dirty` left over from this task would
/// be read as the next one's, and a task that owns nothing would be credited with
/// dirty registers it never wrote.
///
/// # Safety
///
/// `ctx` must point at the live [`TaskContext`] of the task calling this.
pub(super) unsafe fn save_before_switch(ctx: *mut TaskContext) {
    let action = switch_action(read_fs(), false);
    if action.save_outgoing {
        // SAFETY: caller guarantees `ctx` is live; `FS` is `Dirty` here (that is what
        // `save_outgoing` means), so the FP unit is on and `fsd` is legal.
        unsafe { save_registers((&raw mut (*ctx).fp).cast::<u64>()) };
        FP_SAVES.inc();
    }
    write_fs(FpState::Off);
}

/// Hand the FP unit over on the way out of a task that is not coming back.
///
/// No save — a dying task's registers are worth nothing — but the `FS = Off` still
/// matters, and for a reason the save/restore pair does not cover: a task resuming at
/// its *entry point* (freshly spawned, never switched out) never runs
/// [`restore_after_switch`], so whatever `FS` it inherits is whatever it runs with. A
/// leftover `Clean` would give it the FP unit, the previous task's register contents,
/// and no trap into the authority check.
pub(super) fn release_before_exit() {
    write_fs(FpState::Off);
}

/// Put this task's FP registers back, having just been switched *in*.
///
/// The restore is unconditional for an owner — never "only if someone dirtied them".
/// Whatever is in the register file belongs to whichever task ran in between, so
/// skipping the load would not save work, it would disclose another process's data.
///
/// # Safety
///
/// `ctx` must point at the live [`TaskContext`] of the task calling this.
pub(super) unsafe fn restore_after_switch(ctx: *mut TaskContext) {
    let action = switch_action(FpState::Off, current_owns_fp());
    if action.restore_incoming {
        // The unit has to be on before `fld` can run, and `fld` itself promotes `FS`
        // to `Dirty` — so the state we want is written *after* the copy, not before.
        write_fs(FpState::Clean);
        // SAFETY: caller guarantees `ctx` is live, and `FS` was just set non-`Off`.
        unsafe { restore_registers((&raw const (*ctx).fp).cast::<u64>()) };
        FP_RESTORES.inc();
    }
    write_fs(action.incoming_fs);
}
