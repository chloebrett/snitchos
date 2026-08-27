//! JH7110 GMAC (Synopsys `DesignWare` `dwmac-5.20`) register-layout logic — no MMIO.
//!
//! The kernel driver (`kernel/src/device/gmac.rs`) does the volatile reads; what's
//! here is the pure model of *which* offsets and *what the words mean*, transcribed
//! from mainline `dwmac4.h` / `dwmac4_dma.h` / `common.h`. Same split as
//! [`crate::syscrg`]. Design: `docs/vf2-gmac-design.md`.

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

/// What the probe reads from the MAC, in order. The kernel glue walks this and
/// emits one breadcrumb per entry — a loop rather than N transcribed read sites, so
/// that "breadcrumb before each read" holds by construction and a hang localises to
/// the entry whose breadcrumb is last on the wire.
///
/// [`VERSION`] is first; see `the_version_register_is_probed_first`.
pub const PROBE_TARGETS: &[Target] = &[
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
        // one identified the core, so it cannot be read second.
        assert_eq!(PROBE_TARGETS[0].offset, VERSION);
    }

    #[test]
    fn every_target_lies_inside_the_mapped_region() {
        let outside: Vec<_> = PROBE_TARGETS
            .iter()
            .filter(|t| t.offset >= REGION_SIZE)
            .map(|t| t.label)
            .collect();
        assert!(outside.is_empty(), "would fault on read: {outside:?}");
    }

    #[test]
    fn no_two_targets_share_an_offset() {
        // The table is hand-transcribed from mainline headers, where the realistic
        // error is a duplicated line rather than an invented one.
        let dupes: Vec<_> = PROBE_TARGETS
            .iter()
            .enumerate()
            .filter(|(i, t)| PROBE_TARGETS[..*i].iter().any(|p| p.offset == t.offset))
            .map(|(_, t)| t.label)
            .collect();
        assert!(dupes.is_empty(), "duplicated offsets: {dupes:?}");
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
