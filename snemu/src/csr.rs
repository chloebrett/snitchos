//! The control-and-status register file: a sparse store over the CSR
//! addresses snemu models. Addresses outside that set surface as
//! `CsrError::Unknown` (the meta-loop signal), so they're added on demand
//! rather than silently reading as zero.

use std::collections::BTreeMap;

/// CSR addresses (`instr[31:20]`), named per the privileged spec. The kernel
/// runs in S-mode, so these are the Supervisor trap CSRs. Extended as the
/// kernel exercises more of them.
pub(crate) mod addr {
    /// Read-only `time` counter (user-level). Not stored — the CPU answers it
    /// from the instruction-retire count so the clock stays deterministic.
    pub const TIME: u16 = 0xc01;
    pub const SSTATUS: u16 = 0x100;
    pub const SIE: u16 = 0x104;
    pub const STVEC: u16 = 0x105;
    pub const SSCRATCH: u16 = 0x140;
    pub const SEPC: u16 = 0x141;
    pub const SCAUSE: u16 = 0x142;
    pub const STVAL: u16 = 0x143;
    pub const SIP: u16 = 0x144;
    pub const SATP: u16 = 0x180;
    /// Supervisor timer-compare (Sstc). The hart raises a supervisor timer
    /// interrupt once `time >= stimecmp`; the kernel arms it via `csrw 0x14d`.
    pub const STIMECMP: u16 = 0x14d;
}

/// `sstatus` field masks (the S-mode view of the status register).
pub(crate) mod sstatus {
    /// Supervisor interrupt enable.
    pub const SIE: u64 = 1 << 1;
    /// Supervisor previous interrupt enable (holds SIE across a trap).
    pub const SPIE: u64 = 1 << 5;
    /// Supervisor previous privilege (0 = U, 1 = S).
    pub const SPP: u64 = 1 << 8;
}

/// The CSR addresses snemu currently models (the S-mode trap set + satp).
/// Anything else is `Unknown` until added here.
const SUPPORTED: &[u16] = &[
    addr::SSTATUS,
    addr::SIE,
    addr::STVEC,
    addr::SSCRATCH,
    addr::SEPC,
    addr::SCAUSE,
    addr::STVAL,
    addr::SIP,
    addr::SATP,
    addr::STIMECMP,
];

/// A CSR access named an address snemu doesn't model yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CsrError {
    Unknown(u16),
}

/// The control-and-status register file.
pub(crate) struct Csr {
    values: BTreeMap<u16, u64>,
}

impl Csr {
    pub(crate) fn new() -> Self {
        let values = SUPPORTED.iter().map(|&a| (a, 0)).collect();
        Self { values }
    }

    pub(crate) fn read(&self, addr: u16) -> Result<u64, CsrError> {
        self.values
            .get(&addr)
            .copied()
            .ok_or(CsrError::Unknown(addr))
    }

    pub(crate) fn write(&mut self, addr: u16, value: u64) -> Result<(), CsrError> {
        match self.values.get_mut(&addr) {
            Some(slot) => {
                *slot = value;
                Ok(())
            }
            None => Err(CsrError::Unknown(addr)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supported_csrs_default_to_zero_and_round_trip() {
        let mut csr = Csr::new();
        assert_eq!(csr.read(addr::STVEC).unwrap(), 0);
        csr.write(addr::STVEC, 0xdead_beef).unwrap();
        assert_eq!(csr.read(addr::STVEC).unwrap(), 0xdead_beef);
    }

    #[test]
    fn unknown_csr_addresses_are_errors() {
        let mut csr = Csr::new();
        assert_eq!(csr.read(0xfff), Err(CsrError::Unknown(0xfff)));
        assert_eq!(csr.write(0xfff, 1), Err(CsrError::Unknown(0xfff)));
    }
}
