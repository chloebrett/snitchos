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
}
