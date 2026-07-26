//! Trap-cause decoding. Pure bit-twiddling on the `scause` CSR value
//! handed to us by the trap entry assembly — no asm, no CSR reads, so
//! this module lives in `kernel-boot` and is host-testable.

/// Decoded form of the `scause` CSR. The top bit of `scause` is the
/// interrupt-vs-exception flag; the remaining bits are the cause code
/// whose meaning depends on that flag. We name the ones we handle and
/// preserve the raw code for the others.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrapCause {
    SupervisorTimerInterrupt,
    SupervisorExternalInterrupt,
    SupervisorSoftwareInterrupt,
    Breakpoint,
    EnvCallFromUMode,
    EnvCallFromSMode,
    UnknownInterrupt(u64),
    UnknownException(u64),
}

pub fn decode_scause(scause: u64) -> TrapCause {
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

/// What the trap dispatcher should do with a cause it has no dedicated handler
/// for. Returned by [`fault_disposition`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultDisposition {
    /// Terminate the faulting user process; the kernel and every other process
    /// carry on. One user program must never be able to halt the machine.
    TerminateProcess,
    /// A kernel bug (or a platform misconfiguration) with nothing smaller to
    /// terminate — panic, which snitches and halts.
    KernelPanic,
}

/// Decide who dies for an unhandled trap. `from_user` is the privilege the trap
/// came from (`sstatus.SPP == 0`).
///
/// An **exception** is caused by the instruction that was executing, so from
/// U-mode it is the process's fault and the process dies — blanket over every
/// code, so a cause the kernel has never seen can't panic on a user program's
/// behalf. The same exception from S-mode is a kernel bug.
///
/// An **interrupt** is different: the running task merely *was* interrupted, by
/// a source the kernel never enabled. Killing it would pin a kernel or platform
/// misconfiguration on an innocent process, so an unknown interrupt panics from
/// either privilege.
#[must_use]
pub fn fault_disposition(cause: TrapCause, from_user: bool) -> FaultDisposition {
    match cause {
        TrapCause::UnknownInterrupt(_)
        | TrapCause::SupervisorTimerInterrupt
        | TrapCause::SupervisorExternalInterrupt
        | TrapCause::SupervisorSoftwareInterrupt => FaultDisposition::KernelPanic,
        _ if from_user => FaultDisposition::TerminateProcess,
        _ => FaultDisposition::KernelPanic,
    }
}

/// The RISC-V name for an exception cause code, for a human-readable fault
/// report — a log line saying `illegal instruction` beats one saying
/// `scause=0x2`. Callers report the numeric code alongside, so an unnamed code
/// still identifies itself.
#[must_use]
pub fn exception_name(code: u64) -> &'static str {
    match code {
        0 => "instruction address misaligned",
        1 => "instruction access fault",
        2 => "illegal instruction",
        3 => "breakpoint",
        4 => "load address misaligned",
        5 => "load access fault",
        6 => "store/AMO address misaligned",
        7 => "store/AMO access fault",
        8 => "ecall from U-mode",
        9 => "ecall from S-mode",
        12 => "instruction page fault",
        13 => "load page fault",
        15 => "store/AMO page fault",
        _ => "unknown exception",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const INTERRUPT_BIT: u64 = 1 << 63;

    #[test]
    fn timer_interrupt_decodes() {
        assert_eq!(
            decode_scause(INTERRUPT_BIT | 5),
            TrapCause::SupervisorTimerInterrupt,
        );
    }

    #[test]
    fn software_interrupt_decodes() {
        assert_eq!(
            decode_scause(INTERRUPT_BIT | 1),
            TrapCause::SupervisorSoftwareInterrupt,
        );
    }

    #[test]
    fn external_interrupt_decodes() {
        assert_eq!(
            decode_scause(INTERRUPT_BIT | 9),
            TrapCause::SupervisorExternalInterrupt,
        );
    }

    #[test]
    fn breakpoint_exception_decodes() {
        // Top bit clear → exception; code 3 → Breakpoint.
        // Same numeric value (9) as ExternalInterrupt: proves we branch
        // on the interrupt bit before matching the code.
        assert_eq!(decode_scause(3), TrapCause::Breakpoint);
    }

    #[test]
    fn ecall_from_u_and_s_mode_distinguished() {
        assert_eq!(decode_scause(8), TrapCause::EnvCallFromUMode);
        assert_eq!(decode_scause(9), TrapCause::EnvCallFromSMode);
    }

    #[test]
    fn unknown_interrupt_preserves_raw_code() {
        assert_eq!(
            decode_scause(INTERRUPT_BIT | 0x2a),
            TrapCause::UnknownInterrupt(0x2a),
        );
    }

    #[test]
    fn unknown_exception_preserves_raw_code() {
        assert_eq!(decode_scause(99), TrapCause::UnknownException(99));
    }

    // ---- fault_disposition ------------------------------------------------
    //
    // The dispatcher reaches these only for a cause it has no dedicated handler
    // for. The question each answers: whose bug is it, and who dies for it?

    #[test]
    fn unhandled_user_exception_terminates_the_process() {
        // scause=2, illegal instruction — what an FP instruction produces while
        // `sstatus.FS` is Off. One user program must not be able to halt the
        // machine, so this kills the process and the kernel carries on.
        assert_eq!(
            fault_disposition(TrapCause::UnknownException(2), true),
            FaultDisposition::TerminateProcess,
        );
    }

    #[test]
    fn unhandled_kernel_exception_panics() {
        // The same instruction executed in S-mode is a kernel bug, and there is
        // no smaller thing to terminate than the kernel.
        assert_eq!(
            fault_disposition(TrapCause::UnknownException(2), false),
            FaultDisposition::KernelPanic,
        );
    }

    #[test]
    fn every_unhandled_exception_code_is_attributed_to_the_faulting_user() {
        // Not just illegal-instruction: misaligned access, access faults, the
        // page faults, and codes we have no name for all belong to whoever
        // executed them. Blanket, so a newly-seen code can never panic the
        // kernel on a user program's behalf.
        for code in [0, 1, 2, 4, 5, 6, 7, 12, 13, 15, 24, 99] {
            assert_eq!(
                fault_disposition(TrapCause::UnknownException(code), true),
                FaultDisposition::TerminateProcess,
                "exception code {code} from U-mode should terminate the process",
            );
        }
    }

    #[test]
    fn unknown_interrupt_panics_even_from_user_mode() {
        // An interrupt is not attributable to the running task — it was merely
        // *interrupted* by a source the kernel never enabled. Killing the
        // innocent process would hide a real kernel/platform misconfiguration.
        assert_eq!(
            fault_disposition(TrapCause::UnknownInterrupt(7), true),
            FaultDisposition::KernelPanic,
        );
        assert_eq!(
            fault_disposition(TrapCause::UnknownInterrupt(7), false),
            FaultDisposition::KernelPanic,
        );
    }

    #[test]
    fn breakpoint_from_user_terminates_rather_than_panicking() {
        // `Breakpoint` is *named* by `decode_scause` but has no handler — a
        // decoded name must not be mistaken for a handled cause.
        assert_eq!(
            fault_disposition(TrapCause::Breakpoint, true),
            FaultDisposition::TerminateProcess,
        );
    }

    // ---- exception_name ---------------------------------------------------

    #[test]
    fn exception_names_are_readable_in_a_fault_report() {
        // The point of the table: a fault report reads "illegal instruction",
        // not "scause=0x2".
        assert_eq!(exception_name(2), "illegal instruction");
        assert_eq!(exception_name(13), "load page fault");
        assert_eq!(exception_name(6), "store/AMO address misaligned");
    }

    #[test]
    fn unnamed_exception_code_still_yields_a_name() {
        // A code we've never seen must not be an empty string in the log line;
        // the numeric code is reported alongside it by the caller.
        assert_eq!(exception_name(99), "unknown exception");
    }
}
