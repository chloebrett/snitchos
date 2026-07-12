//! The control-and-status register file: a flat store over the CSR
//! addresses snemu models. Addresses outside that set surface as
//! `CsrError::Unknown` (the meta-loop signal), so they're added on demand
//! rather than silently reading as zero.

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
    /// Permit Supervisor User Memory access — S-mode may load/store U-pages only
    /// when this is set (never fetch from them).
    pub const SUM: u64 = 1 << 18;
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

/// The control-and-status register file. Stored as a **flat array** indexed by a
/// jump-table [`slot`](Csr::slot) map, not a `BTreeMap`: `read`/`write` are on the
/// hot path (`pending_interrupt` probes `sip`/`sie`/`sstatus` every instruction),
/// where a tree lookup's comparisons and pointer-chasing measured as a real tax.
/// An array index behind a `match` is O(1) with no indirection, and `Clone` (the
/// snapshot/fork primitive) becomes a cheap array copy. The API is unchanged, so
/// nothing else moves.
#[derive(Clone)]
pub(crate) struct Csr {
    values: [u64; SUPPORTED.len()],
}

impl Csr {
    pub(crate) fn new() -> Self {
        Self { values: [0; SUPPORTED.len()] }
    }

    /// The dense array slot for a modeled CSR, or `None` if unmodeled. A `match`
    /// (jump table), so lookup is a branch + array index — no scan, no tree. Must
    /// cover exactly [`SUPPORTED`]; the round-trip test guards against drift.
    #[inline]
    fn slot(addr: u16) -> Option<usize> {
        Some(match addr {
            addr::SSTATUS => 0,
            addr::SIE => 1,
            addr::STVEC => 2,
            addr::SSCRATCH => 3,
            addr::SEPC => 4,
            addr::SCAUSE => 5,
            addr::STVAL => 6,
            addr::SIP => 7,
            addr::SATP => 8,
            addr::STIMECMP => 9,
            _ => return None,
        })
    }

    /// Fold the CSR file into `h` for the machine state hash — the flat value array
    /// captures every modeled control register.
    pub(crate) fn hash_state(&self, h: &mut impl std::hash::Hasher) {
        std::hash::Hash::hash(&self.values, h);
    }

    pub(crate) fn read(&self, addr: u16) -> Result<u64, CsrError> {
        Self::slot(addr)
            .map(|i| self.values[i])
            .ok_or(CsrError::Unknown(addr))
    }

    pub(crate) fn write(&mut self, addr: u16, value: u64) -> Result<(), CsrError> {
        match Self::slot(addr) {
            Some(i) => {
                self.values[i] = value;
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

    #[test]
    fn every_modeled_csr_defaults_to_zero_and_round_trips_independently() {
        // Guards the address→slot mapping: each modeled CSR must have its own
        // slot (write one, it reads back; the others stay put — no aliasing).
        let mut csr = Csr::new();
        for &a in SUPPORTED {
            assert_eq!(csr.read(a).unwrap(), 0, "{a:#x} defaults to zero");
        }
        for (i, &a) in SUPPORTED.iter().enumerate() {
            csr.write(a, 0x1000 + i as u64).unwrap();
        }
        for (i, &a) in SUPPORTED.iter().enumerate() {
            assert_eq!(csr.read(a).unwrap(), 0x1000 + i as u64, "{a:#x} kept its own value");
        }
    }
}
