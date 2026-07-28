//! Floating-point authority policy: who may execute an FP instruction, decided at
//! the trap that an FP instruction causes.
//!
//! Pure — the kernel supplies the three facts and acts on the verdict. The mechanism
//! is classic lazy-FP context switching, doubling here as the authority check: the
//! illegal-instruction trap that `sstatus.FS == Off` produces *is* the
//! enable-or-refuse decision point, so an integer-only program pays nothing (it never
//! traps) and the robustness fix and the feature land in the same place.
//!
//! Design: `docs/floating-point-design.md`, plan: `plans/floating-point.md`.

/// What to do about a U-mode illegal-instruction trap that might be a request for
/// floating point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FpEnableDecision {
    /// Turn `sstatus.FS` on for this process and **retry** the faulting instruction
    /// (don't advance `sepc`). Snitch the enable — which process, on what authority.
    Enable,
    /// The program's ELF declares the soft-float ABI, so it has no business executing
    /// an FP instruction: refuse and kill it. Unreachable in this tree today (every
    /// binary is built `lp64d`), which is why it is a *test-covered* path rather than
    /// a demonstrated one.
    RefuseUnauthorised,
    /// Another process already holds live FP register state and the kernel cannot yet
    /// save/restore FP registers across a context switch. Refuse loudly rather than
    /// let two processes silently share 32 registers.
    ///
    /// **Interim.** The right answer is FP context switching, at which point this
    /// variant disappears; until then this converts silent cross-process corruption
    /// into an observable refusal, which is the trade this codebase makes everywhere
    /// else (refusals snitch, never silent).
    RefuseBusy,
    /// Not an FP-enable situation: `FS` is already on, so the instruction really is
    /// illegal — or it's an FP instruction this hardware doesn't implement. Falls
    /// through to the ordinary "unhandled user trap kills the process" path.
    NotFpRelated,
}

/// Decide a U-mode illegal-instruction trap.
///
/// - `authorised`: the process's ELF declares a hard-float ABI
///   ([`crate::elf::FloatAbi::uses_hardware_fp`]).
/// - `fs_off`: `sstatus.FS` is `Off` for the faulting context.
/// - `other_holders`: how many *other* processes currently have FP enabled.
///
/// Note the ordering: `fs_off` is checked first. A process that already has FP on
/// cannot reach the enable path at all — its `FS` travels with its trap frame — so a
/// trap with `FS` on is a genuine fault, not a request.
#[must_use]
pub fn enable_decision(authorised: bool, fs_off: bool, other_holders: usize) -> FpEnableDecision {
    if !fs_off {
        return FpEnableDecision::NotFpRelated;
    }
    if !authorised {
        return FpEnableDecision::RefuseUnauthorised;
    }
    if other_holders > 0 {
        return FpEnableDecision::RefuseBusy;
    }
    FpEnableDecision::Enable
}

/// `sstatus.FS` — what the FP unit's registers are worth for a given context.
///
/// The encoding is the hardware's (bits 14:13), and the *ordering* is the point:
/// each state is a stronger claim than the last about what a context switch owes
/// this task. `Off` — no FP state, and an FP instruction traps. `Initial` — FP on,
/// nothing written. `Clean` — registers and the in-memory copy agree. `Dirty` —
/// registers have been written since the copy was made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FpState {
    Off = 0,
    Initial = 1,
    Clean = 2,
    Dirty = 3,
}

/// What a context switch owes the FP unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SwitchAction {
    /// Copy the live FP registers into the outgoing task's own storage.
    pub save_outgoing: bool,
    /// Load the incoming task's FP registers from its storage.
    pub restore_incoming: bool,
    /// What `sstatus.FS` the incoming task runs with.
    pub incoming_fs: FpState,
}

/// Decide what a switch from a task in `outgoing_fs` to one that does
/// (`incoming_owns_fp`) or doesn't hold FP state must do.
///
/// Three rules, each load-bearing for a different reason:
///
/// - **Save only what changed.** `Dirty` is the hardware telling us the registers no
///   longer match the copy. `Clean` and `Initial` mean the copy stands.
/// - **Restore unconditionally for an owner** — never "only if someone dirtied them".
///   Whatever is in the register file belongs to *another process*, so skipping the
///   load is not an optimisation, it is a disclosure. A task's storage starts zeroed,
///   which is what makes the first switch after an enable safe too.
/// - **A non-owner resumes `Off`,** because `Off` is what makes its first FP
///   instruction trap, and that trap is [`enable_decision`]'s only trigger. Resuming a
///   non-owner with FP on grants FP silently and permanently.
#[must_use]
pub fn switch_action(outgoing_fs: FpState, incoming_owns_fp: bool) -> SwitchAction {
    SwitchAction {
        save_outgoing: outgoing_fs == FpState::Dirty,
        restore_incoming: incoming_owns_fp,
        incoming_fs: if incoming_owns_fp { FpState::Clean } else { FpState::Off },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The happy path: an authorised process's first FP instruction turns FP on and
    /// retries. Nothing was declared or asked for — the trap *is* the request.
    #[test]
    fn an_authorised_process_with_fp_off_gets_it_enabled() {
        assert_eq!(enable_decision(true, true, 0), FpEnableDecision::Enable);
    }

    /// A soft-float program executing an FP instruction is refused, not accommodated.
    /// Its ELF says it doesn't use FP registers, so the instruction is either a
    /// mis-build or an attempt to use authority it never claimed.
    #[test]
    fn a_soft_float_process_is_refused() {
        assert_eq!(enable_decision(false, true, 0), FpEnableDecision::RefuseUnauthorised);
    }

    /// **`FS` already on ⇒ the fault is real.** This is the ordering that matters: if
    /// authorisation were checked first, an authorised process would get FP
    /// "re-enabled" on every genuinely-illegal instruction and retry it forever — a
    /// livelock instead of a dead process.
    #[test]
    fn an_illegal_instruction_with_fp_already_on_is_a_real_fault() {
        assert_eq!(enable_decision(true, false, 0), FpEnableDecision::NotFpRelated);
        // ...and being unauthorised doesn't change that: the FP question is settled.
        assert_eq!(enable_decision(false, false, 0), FpEnableDecision::NotFpRelated);
    }

    /// While another process holds FP state, a second request is refused rather than
    /// granted — the interim guard standing in for FP context switching. Without it
    /// the two processes would share one register file and corrupt each other
    /// silently, which is strictly worse than a loud refusal.
    #[test]
    fn a_second_fp_process_is_refused_while_another_holds_the_registers() {
        assert_eq!(enable_decision(true, true, 1), FpEnableDecision::RefuseBusy);
        assert_eq!(enable_decision(true, true, 7), FpEnableDecision::RefuseBusy);
    }

    /// Authorisation outranks the busy check: an unauthorised process is refused for
    /// *its own* reason regardless of who holds the registers, so the snitched reason
    /// names the real problem rather than a coincidence of timing.
    #[test]
    fn an_unauthorised_process_is_refused_for_being_unauthorised_not_for_being_second() {
        assert_eq!(enable_decision(false, true, 3), FpEnableDecision::RefuseUnauthorised);
    }

    /// Only a task that has *written* FP registers needs them copied out. `Dirty` is
    /// precisely that fact, and it is the whole reason `sstatus` tracks Clean/Dirty
    /// rather than a single on/off bit.
    #[test]
    fn a_task_that_wrote_fp_registers_has_them_saved() {
        assert!(switch_action(FpState::Dirty, false).save_outgoing);
    }

    /// `Clean` means the task's in-memory copy still matches the registers, and
    /// `Initial` means it has written nothing since FP was turned on. Saving either
    /// would copy 264 bytes to reproduce what is already there.
    #[test]
    fn a_task_that_only_read_fp_registers_is_not_saved_again() {
        assert!(!switch_action(FpState::Clean, false).save_outgoing);
        assert!(!switch_action(FpState::Initial, false).save_outgoing);
    }

    /// An integer-only task has no FP state to spend a switch on — the common case,
    /// and the one the lazy design exists to keep free.
    #[test]
    fn a_task_with_fp_off_costs_the_switch_nothing() {
        let action = switch_action(FpState::Off, false);
        assert!(!action.save_outgoing);
        assert!(!action.restore_incoming);
    }

    /// The incoming task's registers are loaded from its own copy, and it resumes
    /// `Clean`: the registers and that copy now agree, so a switch straight back out
    /// need not save them.
    #[test]
    fn an_incoming_owner_is_restored_and_resumes_clean() {
        let action = switch_action(FpState::Dirty, true);
        assert!(action.restore_incoming);
        assert_eq!(action.incoming_fs, FpState::Clean);
    }

    /// **The hazard that would silently disable the authority check.** A task with no
    /// FP state must resume with `FS = Off`, because `Off` is what makes its first FP
    /// instruction trap — and that trap *is* the enable-or-refuse decision point
    /// (`enable_decision` above). Resume it `Initial` "to save a trap" and FP is
    /// granted to everyone, forever, unobserved.
    #[test]
    fn a_task_with_no_fp_state_resumes_with_fp_off_so_it_still_traps() {
        assert_eq!(switch_action(FpState::Dirty, false).incoming_fs, FpState::Off);
        assert_eq!(switch_action(FpState::Off, false).incoming_fs, FpState::Off);
    }

    /// **The cross-process leak.** Restoring is unconditional for an owner — it does
    /// not depend on whether the *outgoing* task was saved. The registers an incoming
    /// task would otherwise inherit are another process's values, so a restore skipped
    /// as "nothing changed" hands one process a window onto another's data. The task's
    /// own copy starts zeroed, which is also what makes a first-time enable safe.
    #[test]
    fn an_incoming_owner_is_restored_even_when_nothing_was_saved_on_the_way_out() {
        for outgoing in [FpState::Off, FpState::Initial, FpState::Clean, FpState::Dirty] {
            assert!(
                switch_action(outgoing, true).restore_incoming,
                "leaked {outgoing:?} task's registers into the incoming task"
            );
        }
    }
}
