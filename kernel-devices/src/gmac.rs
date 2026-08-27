//! JH7110 GMAC (Synopsys `DesignWare` `dwmac-5.20`) register-layout logic — no MMIO.
//!
//! The kernel driver (`kernel/src/device/gmac.rs`) does the volatile reads; what's
//! here is the pure model of *which* offsets and *what the words mean*, transcribed
//! from mainline `dwmac4.h` / `dwmac4_dma.h` / `common.h`. Same split as
//! [`crate::syscrg`]. Design: `docs/vf2-gmac-design.md`.

use crate::syscrg;

/// GMAC1 MMIO base (`ethernet@16040000`). GMAC1 rather than GMAC0 because it hangs
/// off SYSCRG, which [`crate::syscrg`] already models — see the design note's
/// comparison. Its 2 MiB megapage (`0x1600_0000`) is not one `kmain` maps; the
/// kernel glue must `insert` it.
pub const BASE: usize = 0x1604_0000;

/// Size of the GMAC register region, from the DT `reg` property.
pub const REGION_SIZE: usize = 0x1_0000;

/// MAC version register. **Offset transcribed as `GMAC4_VERSION` but not confirmed
/// against a datasheet** — which is precisely why it is read first and checked
/// against a known constant: if this word does not identify the core, the offset or
/// the base is wrong and the rest of the dump is fiction.
pub const VERSION: usize = 0x0110;
/// `GMAC_CONFIG` — carries `TE`/`RE`, duplex and speed as U-Boot left them.
pub const CONFIG: usize = 0x0000;
/// `GMAC_MDIO_ADDR` — read for its CSR clock-range field, the MDC divider whose
/// correct value is otherwise a guess off the AHB rate.
pub const MDIO_ADDR: usize = 0x0200;
/// `GMAC_ADDR_HIGH(0)` — the station MAC address U-Boot programmed, high half.
pub const ADDR_HIGH_0: usize = 0x0300;
/// `GMAC_ADDR_LOW(0)` — station MAC address, low half.
pub const ADDR_LOW_0: usize = 0x0304;
/// `DMA_BUS_MODE` — whether the DMA has been reset and how the bus is configured.
pub const DMA_BUS_MODE: usize = 0x1000;
/// `DMA_CHAN_TX_CONTROL` (channel 0) — `ST` says whether a TX engine is running.
pub const DMA_CH0_TX_CONTROL: usize = 0x1104;
/// `DMA_CHAN_TX_BASE_ADDR` (channel 0) — a non-zero ring base means U-Boot left a
/// descriptor ring behind, which says where its buffers live.
pub const DMA_CH0_TX_BASE_ADDR: usize = 0x1114;
/// `DMA_CHAN_TX_RING_LEN` (channel 0).
pub const DMA_CH0_TX_RING_LEN: usize = 0x112C;

/// One register the probe reads and reports. **There is no value or write field**:
/// the probe is read-only by construction rather than by convention, because the
/// state it captures is destroyed by the driver's first reset assert.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Target {
    /// What to call this register in the breadcrumb.
    pub label: &'static str,
    /// Byte offset from [`BASE`].
    pub offset: usize,
}

/// A contiguous MMIO window the probe reads from. Carries its own size so a
/// mistyped offset is a failing test rather than a fault on an unmapped read, and
/// so the kernel glue knows what it must `insert` before touching it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Region {
    /// What to call this window in the breadcrumb.
    pub label: &'static str,
    /// Physical base address.
    pub base: usize,
    /// Window size, from the DT `reg` property.
    pub size: usize,
    /// What to read from it, in order.
    pub targets: &'static [Target],
}

/// What the probe reads, in order. The kernel glue walks this and emits one
/// breadcrumb per entry — a loop rather than N transcribed read sites, so that
/// "breadcrumb before each read" holds by construction and a bus hang localises to
/// the entry whose breadcrumb is last on the wire.
///
/// The MAC comes first because [`VERSION`] is the instrument check; see
/// `the_version_register_is_probed_first`. SYSCRG and `sys_syscon` share the
/// `0x1300_0000` megapage, so the glue needs two inserts, not three.
pub const PROBE_REGIONS: &[Region] = &[GMAC1_REGION, SYSCRG_REGION, SYS_SYSCON_REGION];

/// The MAC's own registers.
pub const GMAC1_REGION: Region = Region {
    label: "gmac1",
    base: BASE,
    size: REGION_SIZE,
    targets: GMAC1_TARGETS,
};

/// Clock gates and reset status for GMAC1 — did U-Boot leave the MAC ungated and
/// out of reset? Offsets are **derived** through [`crate::syscrg`] rather than
/// restated, so this table cannot drift from the controller model the PWMDAC uses.
pub const SYSCRG_REGION: Region = Region {
    label: "syscrg",
    base: syscrg::BASE,
    size: 0x1_0000,
    targets: SYSCRG_TARGETS,
};

/// `sys_syscon` — holds the `phy_intf_sel` field that says which PHY interface the
/// MAC is wired for. U-Boot has already written it correctly.
pub const SYS_SYSCON_REGION: Region = Region {
    label: "sys_syscon",
    base: 0x1303_0000,
    size: 0x1000,
    targets: SYS_SYSCON_TARGETS,
};

/// GMAC1 clock indices (`starfive,jh7110-crg.h`), as the DT's `gmac1` node names
/// them: `pclk`, `stmmaceth`, `ptp_ref`, `tx`, `gtx`.
pub const CLK_GMAC1_AHB: u32 = 97;
/// GMAC1 AXI clock — the DT's `stmmaceth`.
pub const CLK_GMAC1_AXI: u32 = 98;
/// GMAC1 PTP reference clock.
pub const CLK_GMAC1_PTP: u32 = 102;
/// GMAC1 TX clock, inverted variant — where RGMII clock polarity is selected.
pub const CLK_GMAC1_TX_INV: u32 = 106;
/// GMAC1 GTX clock.
pub const CLK_GMAC1_GTXC: u32 = 107;
/// GMAC1 AXI reset line (`JH7110_SYSRST_GMAC1_AXI`).
pub const RST_GMAC1_AXI: u32 = 66;
/// GMAC1 AHB reset line (`JH7110_SYSRST_GMAC1_AHB`).
pub const RST_GMAC1_AHB: u32 = 67;

/// `phy_intf_sel` lives at `sys_syscon + 0x90`, shift 2, three bits wide — the
/// `starfive,syscon = <&sys_syscon 0x90 0x2>` property on the DT's `gmac1` node.
pub const PHY_INTF_SEL: usize = 0x90;

const GMAC1_TARGETS: &[Target] = &[
    Target { label: "version", offset: VERSION },
    Target { label: "mac_config", offset: CONFIG },
    Target { label: "mdio_addr", offset: MDIO_ADDR },
    Target { label: "mac_addr_high", offset: ADDR_HIGH_0 },
    Target { label: "mac_addr_low", offset: ADDR_LOW_0 },
    Target { label: "dma_bus_mode", offset: DMA_BUS_MODE },
    Target { label: "dma_ch0_tx_control", offset: DMA_CH0_TX_CONTROL },
    Target { label: "dma_ch0_tx_base_addr", offset: DMA_CH0_TX_BASE_ADDR },
    Target { label: "dma_ch0_tx_ring_len", offset: DMA_CH0_TX_RING_LEN },
];

const SYSCRG_TARGETS: &[Target] = &[
    Target { label: "clk_gmac1_ahb", offset: syscrg::clock_reg_offset(CLK_GMAC1_AHB) },
    Target { label: "clk_gmac1_axi", offset: syscrg::clock_reg_offset(CLK_GMAC1_AXI) },
    Target { label: "clk_gmac1_ptp", offset: syscrg::clock_reg_offset(CLK_GMAC1_PTP) },
    Target { label: "clk_gmac1_tx_inv", offset: syscrg::clock_reg_offset(CLK_GMAC1_TX_INV) },
    Target { label: "clk_gmac1_gtxc", offset: syscrg::clock_reg_offset(CLK_GMAC1_GTXC) },
    // Both GMAC1 resets share one status word, so one read covers them.
    Target { label: "rst_gmac1_status", offset: syscrg::reset_status_offset(RST_GMAC1_AXI) },
];

const SYS_SYSCON_TARGETS: &[Target] =
    &[Target { label: "phy_intf_sel", offset: PHY_INTF_SEL }];

/// True when both GMAC1 resets read as released. SYSCRG's model is that a status
/// bit reads 1 once its reset is released, so this is the "is the MAC actually out
/// of reset" question the version read cannot answer on its own.
///
/// Unlike [`is_expected_core`] this carries no poison-word guard: SYSCRG is the
/// controller the PWMDAC bring-up already drives on this board, so its liveness is
/// not in question the way a never-touched GMAC window is.
#[must_use]
pub fn gmac1_resets_released(status_word: u32) -> bool {
    // ids 66 and 67 land on bits 2 and 3 of the same word — disjoint, so `|` and
    // `^` agree and mutation testing flags the `| → ^` mutant as a survivor. It is
    // a genuine equivalent mutant, as in `syscrg::pwmdac_bringup`.
    let bits = syscrg::reset_bit(RST_GMAC1_AXI) | syscrg::reset_bit(RST_GMAC1_AHB);
    status_word & bits == bits
}

/// The Synopsys core version, `DWMAC_SNPSVER` — the low byte of the MAC's version
/// register. The high byte is the vendor-defined `DWMAC_USERVER` and is ignored.
#[must_use]
pub fn snps_version(version_word: u32) -> u8 {
    (version_word & 0xFF) as u8
}

/// `DWMAC_CORE_5_20` — what the JH7110's `snps,dwmac-5.20` must report.
pub const DWMAC_CORE_5_20: u8 = 0x52;

/// True when the version register reports the core this board is documented to
/// have. **This is the probe's instrument check**: a wrong base address or a
/// still-asserted reset yields plausible-looking words rather than an error, so
/// nothing else in a dump is believable until this returns true.
#[must_use]
pub fn is_expected_core(version_word: u32) -> bool {
    snps_version(version_word) == DWMAC_CORE_5_20
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    #[test]
    fn the_version_register_is_probed_first() {
        // Not cosmetic ordering: every later word is believed only because this
        // one identified the core, so it cannot be read second — and that means
        // the MAC's own region cannot be probed second either.
        assert_eq!(PROBE_REGIONS[0].base, BASE);
        assert_eq!(PROBE_REGIONS[0].targets[0].offset, VERSION);
    }

    #[test]
    fn every_target_lies_inside_its_region() {
        let outside: Vec<_> = PROBE_REGIONS
            .iter()
            .flat_map(|r| r.targets.iter().map(move |t| (r, t)))
            .filter(|(r, t)| t.offset >= r.size)
            .map(|(_, t)| t.label)
            .collect();
        assert!(outside.is_empty(), "would fault on read: {outside:?}");
    }

    #[test]
    fn no_two_targets_share_an_offset_within_a_region() {
        // The table is hand-transcribed from mainline headers, where the realistic
        // error is a duplicated line rather than an invented one. Across regions a
        // shared offset is meaningless, so this is per-region.
        let dupes: Vec<_> = PROBE_REGIONS
            .iter()
            .flat_map(|r| {
                r.targets
                    .iter()
                    .enumerate()
                    .filter(|(i, t)| r.targets[..*i].iter().any(|p| p.offset == t.offset))
                    .map(|(_, t)| t.label)
            })
            .collect();
        assert!(dupes.is_empty(), "duplicated offsets: {dupes:?}");
    }

    #[test]
    fn gmac1_clock_targets_sit_where_the_documented_indices_put_them() {
        // The table derives these through `syscrg::clock_reg_offset`, so what is
        // worth asserting is the *indices* — a 97/98 transposition is invisible to
        // a derivation that agrees with itself. Literals are computed by hand:
        // one 32-bit control register per clock at `index * 4`.
        let offset_of = |label| {
            SYSCRG_REGION.targets.iter().find(|t| t.label == label).map(|t| t.offset)
        };
        assert_eq!(offset_of("clk_gmac1_ahb"), Some(97 * 4));
        assert_eq!(offset_of("clk_gmac1_axi"), Some(98 * 4));
        assert_eq!(offset_of("clk_gmac1_ptp"), Some(102 * 4));
        assert_eq!(offset_of("clk_gmac1_tx_inv"), Some(106 * 4));
        assert_eq!(offset_of("clk_gmac1_gtxc"), Some(107 * 4));
    }

    #[test]
    fn both_gmac1_resets_land_in_one_status_register() {
        // ids 66 and 67 share a word: 0x308 + (66/32)*4 = 0x310, bits 2 and 3.
        let target = SYSCRG_REGION.targets.iter().find(|t| t.label == "rst_gmac1_status");
        assert_eq!(target.map(|t| t.offset), Some(0x310));
    }

    #[test]
    fn a_reset_reads_as_released_only_when_both_bits_are_set() {
        // SYSCRG's model: status reads 1 once the reset is released.
        assert!(gmac1_resets_released(0b1100));
        assert!(!gmac1_resets_released(0b1000));
        assert!(!gmac1_resets_released(0b0100));
        assert!(!gmac1_resets_released(0));
    }

    #[test]
    fn snps_version_is_the_low_byte() {
        assert_eq!(snps_version(0x1234_5652), 0x52);
    }

    #[test]
    fn the_expected_core_is_dwmac_5_20() {
        assert!(is_expected_core(0x0000_0052));
        // 5.10 is a real DesignWare core, and not the one this board has.
        assert!(!is_expected_core(0x0000_0051));
    }

    #[test]
    fn neither_poison_word_reads_as_the_expected_core() {
        // The two ways a bad read looks: the peripheral held in reset reads as
        // zeroes, a floating or unmapped bus reads as ones. Both must be refused,
        // because every later value in the dump is believed only if this passes.
        assert!(!is_expected_core(0x0000_0000));
        assert!(!is_expected_core(0xFFFF_FFFF));
    }
}
