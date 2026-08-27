//! snemu's model of the JH7110 GMAC1 register window (+ the `sys_syscon` word the
//! GMAC's PHY-mode field lives in) — a synthetic device so `workload=gmac-probe`
//! runs off-hardware. Same reason as [`crate::pwmdac`]: without a device here the
//! guest's access falls through and halts the run (`bus.rs`).
//!
//! **What this proves, and what it cannot.** It exercises the *kernel glue* — that
//! the probe maps its megapages, walks its target table in order, emits a breadcrumb
//! before each read, and reports the version verdict. It says **nothing** about
//! whether the register offsets are right, because it is built from the same
//! mainline headers the driver was: a wrong offset would be wrong identically on
//! both sides and agree. Layout fidelity is settled on hardware (T0–T4) and by the
//! U-Boot `md.l` cross-check in `docs/next-board-session.md`. A green GMAC scenario
//! means "the glue does what we meant", never "the driver works".
//!
//! Read-only by construction, matching the guest: the probe writes nothing, so
//! writes here are swallowed rather than modelled.
//!
//! Two windows (SYSCRG at `0x13020000` is already covered by [`crate::pwmdac`],
//! whose all-ones read makes every clock enabled and every reset released):
//! - **GMAC1** `0x16040000` — the version register answers [`SNPSVER_5_20`]; every
//!   other offset reads 0, which is what a MAC U-Boot has not configured looks like.
//! - **`sys_syscon`** `0x13030000` — the `phy_intf_sel` word.

/// GMAC1 (`ethernet@16040000`).
const GMAC1_BASE: u64 = 0x1604_0000;
const GMAC1_SIZE: u64 = 0x1_0000;

/// `sys_syscon` — holds `phy_intf_sel` at +0x90.
const SYS_SYSCON_BASE: u64 = 0x1303_0000;
const SYS_SYSCON_SIZE: u64 = 0x1000;

/// Offset of the MAC version register within the GMAC window.
const VERSION_OFFSET: u64 = 0x0110;

/// What the version register reports: `DWMAC_CORE_5_20` in the low byte, with a
/// vendor `USERVER` in the high byte so the guest's masking is actually exercised
/// rather than passing on a bare `0x52`.
pub(crate) const SNPSVER_5_20: u64 = 0x0000_1052;

/// `phy_intf_sel` = RGMII (1) at shift 2, i.e. the field the board's `rgmii-id`
/// mode implies. Arbitrary but non-zero: a zero here would be indistinguishable
/// from an unmodelled window.
const PHY_INTF_SEL_WORD: u64 = 0b001 << 2;

/// Whether `addr` is in the GMAC1 or `sys_syscon` window.
pub(crate) fn in_window(addr: u64) -> bool {
    (GMAC1_BASE..GMAC1_BASE + GMAC1_SIZE).contains(&addr)
        || (SYS_SYSCON_BASE..SYS_SYSCON_BASE + SYS_SYSCON_SIZE).contains(&addr)
}

/// Read semantics. Depends only on `addr` — there is no captured state, because the
/// guest only reads.
pub(crate) fn read(addr: u64) -> u64 {
    if addr == GMAC1_BASE + VERSION_OFFSET {
        return SNPSVER_5_20;
    }
    if addr == SYS_SYSCON_BASE + 0x90 {
        return PHY_INTF_SEL_WORD;
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_version_register_identifies_a_dwmac_5_20() {
        // The low byte is what `kernel_devices::gmac::is_expected_core` checks.
        assert_eq!(read(GMAC1_BASE + VERSION_OFFSET) & 0xFF, 0x52);
    }

    #[test]
    fn the_version_word_carries_a_vendor_byte_above_the_core_id() {
        // So a guest that forgot to mask fails here rather than on the board.
        assert_ne!(read(GMAC1_BASE + VERSION_OFFSET), 0x52);
    }

    #[test]
    fn an_unconfigured_register_reads_zero() {
        // A MAC U-Boot never touched. Notably this is *not* all-ones: the guest's
        // poison-word check treats 0xFFFF_FFFF as a floating bus, and a model that
        // returned it would make every probe run look like a hardware fault.
        assert_eq!(read(GMAC1_BASE), 0);
        assert_eq!(read(GMAC1_BASE + 0x1114), 0);
    }

    #[test]
    fn the_phy_interface_field_is_non_zero() {
        assert_ne!(read(SYS_SYSCON_BASE + 0x90), 0);
    }

    #[test]
    fn both_windows_are_claimed_and_neighbours_are_not() {
        assert!(in_window(GMAC1_BASE));
        assert!(in_window(GMAC1_BASE + GMAC1_SIZE - 1));
        assert!(in_window(SYS_SYSCON_BASE));
        assert!(in_window(SYS_SYSCON_BASE + SYS_SYSCON_SIZE - 1));
        assert!(!in_window(GMAC1_BASE - 1), "GMAC0's window is not modelled");
        assert!(!in_window(GMAC1_BASE + GMAC1_SIZE));
        assert!(!in_window(SYS_SYSCON_BASE + SYS_SYSCON_SIZE));
    }
}
