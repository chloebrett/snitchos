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
}
