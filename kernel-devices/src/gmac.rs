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

/// An Sv39 megapage — the granularity `kmain` maps MMIO at.
pub const MEGAPAGE_SIZE: usize = 0x20_0000;

/// The megapages the kernel glue must `insert` before walking [`PROBE_REGIONS`].
/// **Two, not three**: SYSCRG and `sys_syscon` are `0x1302_0000` and `0x1303_0000`,
/// which share one. Neither is a megapage `kmain` already maps.
///
/// Stated rather than computed because the probe runs before there is a heap to
/// deduplicate into; `every_region_is_covered_by_a_declared_megapage` and
/// `no_declared_megapage_is_redundant` check it against [`PROBE_REGIONS`] in both
/// directions, so it cannot silently drift or over-map.
pub const PROBE_MEGAPAGES: &[usize] = &[0x1600_0000, 0x1300_0000];

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

/// `TDES2_BUFFER1_SIZE_MASK` — the buffer length field, `GENMASK(13, 0)`. Its width
/// is the hard cap on a single-descriptor frame.
pub const TDES2_BUFFER1_SIZE_MASK: u32 = (1 << 14) - 1;
/// `TDES2_INTERRUPT_ON_COMPLETION`. Left clear: completion is polled off the
/// heartbeat, so the TX path routes no PLIC interrupt at all.
pub const TDES2_IOC: u32 = 1 << 31;
/// `TDES3_PACKET_SIZE_MASK` — total frame length, `GENMASK(14, 0)`.
pub const TDES3_PACKET_SIZE_MASK: u32 = (1 << 15) - 1;
/// `TDES3_LAST_DESCRIPTOR`.
pub const TDES3_LD: u32 = 1 << 28;
/// `TDES3_FIRST_DESCRIPTOR`. One frame is one descriptor here, so `FD` and `LD` are
/// always set together — no scatter-gather, no TSO.
pub const TDES3_FD: u32 = 1 << 29;
/// `TDES3_CONTEXT_TYPE`. Always clear: a context descriptor carries timestamps and
/// TSO parameters, neither of which this driver uses.
pub const TDES3_CTXT: u32 = 1 << 30;
/// `TDES3_OWN` — set means the device owns the descriptor. **Written last, always.**
pub const TDES3_OWN: u32 = 1 << 31;

/// One `dwmac4` transmit descriptor: four little-endian words the device reads by
/// DMA. `#[repr(C)]` because the layout is the hardware's, not Rust's.
///
/// **The ownership handoff is a separate operation by construction.** [`prepare`]
/// builds the body and leaves [`TDES3_OWN`] clear; [`give_to_device`] sets it. There
/// is no way to write both in one step, because a device that observes `OWN` over a
/// half-written descriptor transmits garbage — silently, intermittently, and from a
/// site three layers from the symptom. Making that impossible in the type is
/// cheaper than a comment asking the next reader to be careful.
///
/// The kernel glue still owes a `fence` between the body write and the `OWN` write,
/// and another before the tail-pointer kick: ordering the *operations* is all a pure
/// type can do, and RISC-V gives no ordering between plain stores.
///
/// [`prepare`]: Descriptor::prepare
/// [`give_to_device`]: Descriptor::give_to_device
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct Descriptor {
    /// Buffer 1 physical address, low 32 bits.
    pub tdes0: u32,
    /// Buffer 1 physical address, high 32 bits. Board RAM starts at `0x4000_0000`
    /// and runs 4 GiB, so a physical address genuinely can exceed 32 bits.
    pub tdes1: u32,
    /// Buffer length in `[13:0]`; `IOC` at bit 31.
    pub tdes2: u32,
    /// Control and frame length — and `OWN` at bit 31.
    pub tdes3: u32,
}

impl Descriptor {
    /// Build a single-descriptor frame of `len` bytes at physical address
    /// `buffer_pa`, **not yet owned by the device**.
    ///
    /// `None` when `len` is zero or wider than [`TDES2_BUFFER1_SIZE_MASK`] — refused
    /// rather than truncated, because a silently shortened frame leaves the board
    /// malformed and is diagnosed nowhere near here.
    #[must_use]
    pub fn prepare(buffer_pa: u64, len: u32) -> Option<Self> {
        if len == 0 || len > TDES2_BUFFER1_SIZE_MASK {
            return None;
        }
        Some(Self {
            tdes0: (buffer_pa & 0xFFFF_FFFF) as u32,
            tdes1: (buffer_pa >> 32) as u32,
            tdes2: len & TDES2_BUFFER1_SIZE_MASK,
            // `FD` (29), `LD` (28) and the masked length (`[14:0]`) are disjoint, so
            // `|` and `^` agree and mutation testing reports the `| → ^` mutants as
            // survivors. Genuine equivalent mutants, as in `syscrg::pwmdac_bringup`.
            tdes3: TDES3_FD | TDES3_LD | (len & TDES3_PACKET_SIZE_MASK),
        })
    }

    /// Hand the descriptor to the device by setting [`TDES3_OWN`]. The caller must
    /// have written the body to memory first, with a `fence` between.
    #[must_use]
    pub fn give_to_device(self) -> Self {
        Self { tdes3: self.tdes3 | TDES3_OWN, ..self }
    }

    /// Whether the device still owns this descriptor. Clearing `OWN` is how the MAC
    /// reports completion — the reclaim signal, and (at T3) the first proof the DMA
    /// engine read anything at all.
    #[must_use]
    pub fn is_owned_by_device(&self) -> bool {
        self.tdes3 & TDES3_OWN != 0
    }
}

/// `GMAC_MDIO_DATA` — the transaction's data word, low 16 bits.
pub const MDIO_DATA: usize = 0x0204;

/// `GBUSY` — set by software to start a transaction, cleared by the MAC when the
/// PHY has answered. (Written as a shift for symmetry with its neighbours; mutation
/// testing flags `<< → >>` here, which is equivalent at a shift of zero.)
pub const MDIO_BUSY: u32 = 1 << 0;
/// Operation-command shift within [`MDIO_ADDR`].
const MDIO_GOC_SHIFT: u32 = 2;
/// `MII_GMAC4_WRITE`.
const MDIO_GOC_WRITE: u32 = 1 << MDIO_GOC_SHIFT;
/// `MII_GMAC4_READ`.
const MDIO_GOC_READ: u32 = 3 << MDIO_GOC_SHIFT;
/// CSR clock-range field shift — the MDC divider off the AHB clock.
const MDIO_CSR_SHIFT: u32 = 8;
/// Register-address (`RDA`) field shift.
const MDIO_REG_SHIFT: u32 = 16;
/// PHY-address (`PA`) field shift.
const MDIO_PHY_SHIFT: u32 = 21;

/// How many times [`Mdio`] reads `GBUSY` before giving up. Mainline polls at 100 µs
/// with a 10 ms ceiling; this is that ratio.
///
/// **The bound is not a tuning knob, it is the safety property.** An unbounded poll
/// against a PHY held in reset never returns, and a kernel that never returns has
/// nothing to say about why — the `PollUntilSet` hang the board watchdog exists to
/// catch. Bounded here, in a layer where a test can prove it.
pub const MDIO_MAX_POLLS: u32 = 100;

/// Why an MDIO transaction failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MdioError {
    /// `GBUSY` never cleared within [`MDIO_MAX_POLLS`]. The PHY is held in reset,
    /// absent, or the MDC divider is wrong.
    Timeout,
    /// A PHY or register address wider than [`MDIO_ADDR_BITS`].
    BadAddress,
}

/// MDIO addresses are 5 bits wide. Enforced rather than masked: a caller that means
/// `0x20` means something, and silently wrapping it to `0` would address a different
/// PHY than the one asked for.
pub const MDIO_ADDR_BITS: u8 = 5;

/// The MAC's register file, as the kernel glue's volatile accesses. The seam that
/// keeps this crate free of MMIO — same shape as [`crate::virtio::MmioTransport`].
pub trait GmacTransport {
    /// Read the 32-bit register at `offset` from the MAC's base.
    fn read_reg(&self, offset: usize) -> u32;
    /// Write the 32-bit register at `offset` from the MAC's base.
    fn write_reg(&mut self, offset: usize, value: u32);
}

/// MDIO transaction sequencing over a [`GmacTransport`], carrying the CSR
/// clock-range field that sets the MDC divider.
///
/// That divider is the single most likely constant to be wrong on first contact
/// with the board: too fast and the PHY answers garbage that looks like a wiring
/// fault. It is a constructor parameter rather than a constant so a bring-up
/// session can sweep it without a rebuild of this crate's tests.
#[derive(Clone, Copy, Debug)]
pub struct Mdio {
    csr_clock_range: u32,
}

impl Mdio {
    /// `csr_clock_range` is the CSR-clock-range field value for the AHB rate.
    #[must_use]
    pub const fn new(csr_clock_range: u32) -> Self {
        Self { csr_clock_range }
    }

    /// Validated so [`address_word`]'s fields are provably disjoint.
    ///
    /// [`address_word`]: Mdio::address_word
    fn check(phy: u8, reg: u8) -> Result<(), MdioError> {
        let limit = 1 << MDIO_ADDR_BITS;
        if phy >= limit || reg >= limit {
            return Err(MdioError::BadAddress);
        }
        Ok(())
    }

    /// `PA`, `RDA`, the CSR range, the opcode and `GBUSY` occupy disjoint bits —
    /// guaranteed by [`check`], which is why mutation testing reports the `| → ^`
    /// mutants here as survivors. They are genuine equivalent mutants, as in
    /// `syscrg::pwmdac_bringup`. Without that validation they would *not* be
    /// equivalent, which is what made the check worth adding.
    ///
    /// [`check`]: Mdio::check
    fn address_word(self, phy: u8, reg: u8, operation: u32) -> u32 {
        (u32::from(phy) << MDIO_PHY_SHIFT)
            | (u32::from(reg) << MDIO_REG_SHIFT)
            | (self.csr_clock_range << MDIO_CSR_SHIFT)
            | operation
            | MDIO_BUSY
    }

    /// Poll `GBUSY` until the MAC clears it, bounded by [`MDIO_MAX_POLLS`].
    fn await_completion<T: GmacTransport>(t: &T) -> Result<(), MdioError> {
        for _ in 0..MDIO_MAX_POLLS {
            if t.read_reg(MDIO_ADDR) & MDIO_BUSY == 0 {
                return Ok(());
            }
        }
        Err(MdioError::Timeout)
    }

    /// Read one PHY register. Returns the low 16 bits of the data register.
    pub fn read<T: GmacTransport>(self, t: &mut T, phy: u8, reg: u8) -> Result<u16, MdioError> {
        Self::check(phy, reg)?;
        t.write_reg(MDIO_ADDR, self.address_word(phy, reg, MDIO_GOC_READ));
        Self::await_completion(t)?;
        Ok((t.read_reg(MDIO_DATA) & 0xFFFF) as u16)
    }

    /// Write one PHY register.
    ///
    /// The data word goes down **before** the address word, because writing the
    /// address is what starts the transaction — reversing them races the PHY
    /// against a data register that has not been loaded yet.
    pub fn write<T: GmacTransport>(
        self,
        t: &mut T,
        phy: u8,
        reg: u8,
        value: u16,
    ) -> Result<(), MdioError> {
        Self::check(phy, reg)?;
        t.write_reg(MDIO_DATA, u32::from(value));
        t.write_reg(MDIO_ADDR, self.address_word(phy, reg, MDIO_GOC_WRITE));
        Self::await_completion(t)
    }
}

/// Why a submission was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TxError {
    /// Every usable slot is outstanding. The caller drops the frame; telemetry is
    /// fire-and-forget and blocking the emitter would be worse than losing a frame.
    Full,
    /// Zero-length, or longer than [`TDES2_BUFFER1_SIZE_MASK`].
    BadLength,
}

/// A circular transmit descriptor ring of `N` slots — pure bookkeeping over the
/// descriptor array the device DMAs from. No MMIO, no alloc.
///
/// **Usable capacity is `N - 1`, not `N`.** The device walks from its own position
/// to wherever the tail pointer says, so a head that wrapped all the way onto the
/// reclaim point would read as "nothing to do" and lose the entire ring rather than
/// overflow it. One slot always stays free.
///
/// **Reclaim is split across the crate boundary on purpose.** This ring lives in the
/// static the device writes into, so an ownership check has to be a *volatile* read
/// — a notion `kernel-devices` deliberately does not have. So the glue drives
/// [`peek_reclaimable`] / [`release_one`] around its own volatile read, and the
/// ordering rule (stop at the first slot still owned) stays here, tested.
///
/// [`peek_reclaimable`]: TxRing::peek_reclaimable
/// [`release_one`]: TxRing::release_one
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct TxRing<const N: usize> {
    /// The descriptor array itself — what the device reads. `repr(C)` and first, so
    /// the glue can hand the device this field's physical address.
    descriptors: [Descriptor; N],
    head: usize,
    reclaim: usize,
    outstanding: usize,
}

impl<const N: usize> Default for TxRing<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> TxRing<N> {
    /// An empty ring. `const` so it can live in a `static` without an initialiser
    /// running at boot.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            descriptors: [Descriptor { tdes0: 0, tdes1: 0, tdes2: 0, tdes3: 0 }; N],
            head: 0,
            reclaim: 0,
            outstanding: 0,
        }
    }

    /// Usable slots — one fewer than `N`; see the type's note.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        N - 1
    }

    /// How many slots the device has not yet returned.
    #[must_use]
    pub const fn outstanding(&self) -> usize {
        self.outstanding
    }

    /// Write a frame's descriptor body into the next free slot and return that slot.
    /// **Does not hand it to the device** — call [`publish`] after fencing.
    ///
    /// [`publish`]: TxRing::publish
    pub fn submit(&mut self, buffer_pa: u64, len: u32) -> Result<usize, TxError> {
        if self.outstanding >= self.capacity() {
            return Err(TxError::Full);
        }
        let descriptor = Descriptor::prepare(buffer_pa, len).ok_or(TxError::BadLength)?;
        let slot = self.head;
        self.descriptors[slot] = descriptor;
        self.head = (self.head + 1) % N;
        self.outstanding += 1;
        Ok(slot)
    }

    /// Set `OWN` on a slot, handing it to the device.
    pub fn publish(&mut self, slot: usize) {
        self.descriptors[slot] = self.descriptors[slot].give_to_device();
    }

    /// The slot the glue should check next, or `None` when nothing is outstanding.
    /// The glue reads its `OWN` bit volatilely and calls [`release_one`] only if the
    /// device has cleared it.
    ///
    /// [`release_one`]: TxRing::release_one
    #[must_use]
    pub const fn peek_reclaimable(&self) -> Option<usize> {
        if self.outstanding == 0 {
            None
        } else {
            Some(self.reclaim)
        }
    }

    /// Take back the slot [`peek_reclaimable`] named. Completion is in order, so the
    /// glue must stop at the first slot the device still owns rather than scanning
    /// past it — releasing a live descriptor reuses its buffer mid-transmit.
    ///
    /// [`peek_reclaimable`]: TxRing::peek_reclaimable
    pub fn release_one(&mut self) {
        if self.outstanding == 0 {
            return;
        }
        self.reclaim = (self.reclaim + 1) % N;
        self.outstanding -= 1;
    }

    /// Read a slot's descriptor. The glue uses the physical address of the ring for
    /// DMA; this is for assertions and for the reclaim check's non-volatile half.
    #[must_use]
    pub const fn descriptor(&self, slot: usize) -> &Descriptor {
        &self.descriptors[slot]
    }

    /// Clear a slot's `OWN` bit, standing in for the device completing it. Tests
    /// only — on hardware the MAC does this by DMA.
    #[cfg(test)]
    fn simulate_completion(&mut self, slot: usize) {
        self.descriptors[slot].tdes3 &= !TDES3_OWN;
    }
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

    fn covers(megapage: usize, r: &Region) -> bool {
        r.base >= megapage && r.base + r.size <= megapage + MEGAPAGE_SIZE
    }

    #[test]
    fn every_region_is_covered_by_a_declared_megapage() {
        let uncovered: Vec<_> = PROBE_REGIONS
            .iter()
            .filter(|r| !PROBE_MEGAPAGES.iter().any(|m| covers(*m, r)))
            .map(|r| r.label)
            .collect();
        assert!(uncovered.is_empty(), "would fault: no mapping for {uncovered:?}");
    }

    #[test]
    fn no_declared_megapage_is_redundant() {
        // Mapping more than the probe reads is authority it has no reason to hold,
        // and the claim "two inserts, not three" is only true if this passes.
        let redundant: Vec<_> = PROBE_MEGAPAGES
            .iter()
            .enumerate()
            .filter(|(i, _)| {
                let others: Vec<_> =
                    PROBE_MEGAPAGES.iter().enumerate().filter(|(j, _)| j != i).collect();
                PROBE_REGIONS
                    .iter()
                    .all(|r| others.iter().any(|(_, m)| covers(**m, r)))
            })
            .map(|(_, m)| *m)
            .collect();
        assert!(redundant.is_empty(), "unnecessary mappings: {redundant:x?}");
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
    fn a_prepared_descriptor_splits_the_buffer_address() {
        let d = Descriptor::prepare(0x1_4000_5678, 64).expect("64 fits");
        assert_eq!(d.tdes0, 0x4000_5678, "low half");
        assert_eq!(d.tdes1, 0x1, "high half — board RAM reaches past 4 GiB");
    }

    #[test]
    fn a_prepared_descriptor_carries_the_length_in_both_places() {
        let d = Descriptor::prepare(0x4020_0000, 1514).expect("1514 fits");
        assert_eq!(d.tdes2 & 0x3FFF, 1514, "buffer size, TDES2[13:0]");
        assert_eq!(d.tdes3 & 0x7FFF, 1514, "frame length, TDES3[14:0]");
    }

    #[test]
    fn a_prepared_descriptor_is_one_whole_frame() {
        let d = Descriptor::prepare(0x4020_0000, 60).expect("60 fits");
        assert!(d.tdes3 & TDES3_FD != 0, "first segment");
        assert!(d.tdes3 & TDES3_LD != 0, "last segment");
        assert_eq!(d.tdes3 & TDES3_CTXT, 0, "a normal descriptor, not a context one");
        assert_eq!(d.tdes2 & TDES2_IOC, 0, "we poll for completion; no interrupt");
    }

    #[test]
    fn preparing_a_descriptor_never_sets_the_ownership_bit() {
        // The whole point of the two-phase type. If `prepare` could hand the device
        // a descriptor, a caller could publish one before its body was written —
        // a real, silent, hard-to-reproduce hardware race.
        let d = Descriptor::prepare(0x4020_0000, 64).expect("64 fits");
        assert_eq!(d.tdes3 & TDES3_OWN, 0);
        assert_eq!(d.give_to_device().tdes3 & TDES3_OWN, TDES3_OWN);
    }

    #[test]
    fn the_descriptor_bit_constants_sit_where_the_spec_puts_them() {
        // Named masks keep the encoding readable, but `x & MASK == 0` passes
        // vacuously if MASK is ever zero — and so does `x & MASK == MASK`. Pinning
        // the literals independently of the shifts that build them is what makes
        // every other assertion in this module mean something.
        assert_eq!(TDES2_BUFFER1_SIZE_MASK, 0x0000_3FFF);
        assert_eq!(TDES2_IOC, 0x8000_0000);
        assert_eq!(TDES3_PACKET_SIZE_MASK, 0x0000_7FFF);
        assert_eq!(TDES3_LD, 0x1000_0000);
        assert_eq!(TDES3_FD, 0x2000_0000);
        assert_eq!(TDES3_CTXT, 0x4000_0000);
        assert_eq!(TDES3_OWN, 0x8000_0000);
    }

    #[test]
    fn handing_over_a_descriptor_twice_leaves_it_owned() {
        // Not pedantry: `|` is idempotent and `^` is not, and a reclaim path that
        // re-published a descriptor would otherwise silently un-hand it.
        let handed = Descriptor::prepare(0x4020_0000, 64).expect("64 fits").give_to_device();
        assert!(handed.give_to_device().is_owned_by_device());
    }

    #[test]
    fn a_frame_too_long_for_the_size_field_is_refused() {
        // Refused, not truncated: a silently shortened frame goes out malformed and
        // looks like a driver bug three layers away.
        assert!(Descriptor::prepare(0x4020_0000, 0x3FFF).is_some(), "the largest that fits");
        assert!(Descriptor::prepare(0x4020_0000, 0x4000).is_none());
    }

    #[test]
    fn a_zero_length_frame_is_refused() {
        assert!(Descriptor::prepare(0x4020_0000, 0).is_none());
    }

    #[test]
    fn the_device_returns_a_descriptor_by_clearing_ownership() {
        let ours = Descriptor::prepare(0x4020_0000, 64).expect("64 fits");
        assert!(!ours.is_owned_by_device(), "not handed over yet");
        assert!(ours.give_to_device().is_owned_by_device());
    }

    fn ring() -> TxRing<4> {
        TxRing::new()
    }

    #[test]
    fn a_fresh_ring_has_nothing_outstanding() {
        let r = ring();
        assert_eq!(r.outstanding(), 0);
        assert_eq!(r.peek_reclaimable(), None);
    }

    #[test]
    fn a_submission_fills_the_next_slot_and_advances() {
        let mut r = ring();
        assert_eq!(r.submit(0x4020_0000, 64), Ok(0));
        assert_eq!(r.submit(0x4020_1000, 64), Ok(1));
        assert_eq!(r.outstanding(), 2);
    }

    #[test]
    fn a_submitted_slot_is_not_yet_owned_by_the_device() {
        // The two-phase discipline survives the ring: `submit` writes the body,
        // `publish` hands it over, and the glue fences between them.
        let mut r = ring();
        let slot = r.submit(0x4020_0000, 64).expect("empty ring");
        assert!(!r.descriptor(slot).is_owned_by_device());
        r.publish(slot);
        assert!(r.descriptor(slot).is_owned_by_device());
    }

    #[test]
    fn the_ring_refuses_a_submission_that_would_catch_the_reclaim_point() {
        // One slot always stays free. The device walks from its own position to the
        // tail pointer, so a head that wrapped onto the reclaim point would read as
        // "nothing to do" and lose the whole ring rather than overflowing it.
        let mut r = ring();
        assert_eq!(r.capacity(), 3, "N - 1");
        for _ in 0..3 {
            r.submit(0x4020_0000, 64).expect("within capacity");
        }
        assert_eq!(r.submit(0x4020_0000, 64), Err(TxError::Full));
    }

    #[test]
    fn a_refused_submission_consumes_nothing() {
        let mut r = ring();
        for _ in 0..3 {
            r.submit(0x4020_0000, 64).expect("within capacity");
        }
        let _ = r.submit(0x4020_0000, 64);
        assert_eq!(r.outstanding(), 3, "a refusal must not advance the head");
    }

    #[test]
    fn a_bad_length_is_refused_without_consuming_a_slot() {
        let mut r = ring();
        assert_eq!(r.submit(0x4020_0000, 0), Err(TxError::BadLength));
        assert_eq!(r.outstanding(), 0);
    }

    #[test]
    fn slots_wrap_once_the_device_returns_them() {
        let mut r = ring();
        for _ in 0..3 {
            let slot = r.submit(0x4020_0000, 64).expect("within capacity");
            r.publish(slot);
        }
        // The device completes all three.
        for slot in 0..3 {
            r.simulate_completion(slot);
            r.release_one();
        }
        assert_eq!(r.outstanding(), 0);
        assert_eq!(r.submit(0x4020_0000, 64), Ok(3), "the last free slot");
        assert_eq!(r.submit(0x4020_0000, 64), Ok(0), "then wraps");
    }

    #[test]
    fn reclaim_stops_at_the_first_slot_the_device_still_owns() {
        // Completion is in order, so a scan that ran past a still-owned descriptor
        // would free a live one — the buffer gets reused mid-transmit.
        let mut r = ring();
        for _ in 0..3 {
            let slot = r.submit(0x4020_0000, 64).expect("within capacity");
            r.publish(slot);
        }
        r.simulate_completion(0);
        assert_eq!(r.peek_reclaimable(), Some(0));
        r.release_one();
        assert_eq!(r.peek_reclaimable(), Some(1), "still owned — the glue stops here");
        assert!(r.descriptor(1).is_owned_by_device());
        assert_eq!(r.outstanding(), 2);
    }

    /// A PHY that answers after `busy_reads` polls. `Cell` because `read_reg` takes
    /// `&self` — an MMIO read is a read, even when the device changes under it.
    struct FakePhy {
        writes: core::cell::RefCell<Vec<(usize, u32)>>,
        busy_reads: core::cell::Cell<u32>,
        reads: core::cell::Cell<u32>,
        answer: u32,
    }

    impl FakePhy {
        fn answering_after(busy_reads: u32, answer: u32) -> Self {
            Self {
                writes: core::cell::RefCell::new(Vec::new()),
                busy_reads: core::cell::Cell::new(busy_reads),
                reads: core::cell::Cell::new(0),
                answer,
            }
        }

        fn never_answers() -> Self {
            Self::answering_after(u32::MAX, 0)
        }

        fn written(&self, offset: usize) -> Option<u32> {
            self.writes.borrow().iter().rev().find(|(o, _)| *o == offset).map(|(_, v)| *v)
        }

        fn write_order(&self) -> Vec<usize> {
            self.writes.borrow().iter().map(|(o, _)| *o).collect()
        }
    }

    impl GmacTransport for FakePhy {
        fn read_reg(&self, offset: usize) -> u32 {
            if offset == MDIO_DATA {
                return self.answer;
            }
            self.reads.set(self.reads.get() + 1);
            let remaining = self.busy_reads.get();
            if remaining == 0 {
                return 0;
            }
            self.busy_reads.set(remaining.saturating_sub(1));
            MDIO_BUSY
        }

        fn write_reg(&mut self, offset: usize, value: u32) {
            self.writes.borrow_mut().push((offset, value));
        }
    }

    #[test]
    fn a_read_builds_the_address_word_the_spec_describes() {
        let mut phy = FakePhy::answering_after(0, 0);
        Mdio::new(4).read(&mut phy, 0, 2).expect("answers immediately");
        // PA 0<<21 | RDA 2<<16 | CSR 4<<8 | GOC READ 3<<2 | GBUSY
        assert_eq!(phy.written(MDIO_ADDR), Some(0x0002_040D));
    }

    #[test]
    fn a_write_builds_its_own_address_word_and_lands_the_value_first() {
        let mut phy = FakePhy::answering_after(0, 0);
        Mdio::new(4).write(&mut phy, 1, 0, 0x1234).expect("answers immediately");
        // PA 1<<21 | RDA 0 | CSR 4<<8 | GOC WRITE 1<<2 | GBUSY
        assert_eq!(phy.written(MDIO_ADDR), Some(0x0020_0405));
        assert_eq!(phy.written(MDIO_DATA), Some(0x1234));
        // Ordering is the point: writing ADDR is what *starts* the transaction, so
        // the data has to be in place before it, not after.
        assert_eq!(phy.write_order(), [MDIO_DATA, MDIO_ADDR]);
    }

    #[test]
    fn an_address_wider_than_five_bits_is_refused_before_touching_the_device() {
        // MDIO addresses are 5 bits. A wider one would shift PA into RDA's field
        // and silently address a different register on a different PHY — refuse it
        // rather than start a corrupt transaction.
        let mut phy = FakePhy::answering_after(0, 0);
        assert_eq!(Mdio::new(4).read(&mut phy, 32, 0), Err(MdioError::BadAddress));
        assert_eq!(Mdio::new(4).read(&mut phy, 0, 32), Err(MdioError::BadAddress));
        assert_eq!(Mdio::new(4).write(&mut phy, 32, 0, 1), Err(MdioError::BadAddress));
        assert!(phy.write_order().is_empty(), "a refusal must not write anything");
    }

    #[test]
    fn the_widest_valid_address_is_still_accepted() {
        let mut phy = FakePhy::answering_after(0, 0);
        Mdio::new(4).read(&mut phy, 31, 31).expect("31 is a valid 5-bit address");
        // PA 31<<21 | RDA 31<<16 | CSR 4<<8 | GOC READ | GBUSY — adjacent, disjoint.
        assert_eq!(phy.written(MDIO_ADDR), Some(0x03FF_040D));
    }

    #[test]
    fn a_read_returns_the_low_half_of_the_data_register() {
        let mut phy = FakePhy::answering_after(0, 0xDEAD_BEEF);
        assert_eq!(Mdio::new(4).read(&mut phy, 0, 2), Ok(0xBEEF));
    }

    #[test]
    fn a_read_waits_for_a_phy_that_answers_slowly() {
        let mut phy = FakePhy::answering_after(5, 0x00AB);
        assert_eq!(Mdio::new(4).read(&mut phy, 0, 2), Ok(0xAB));
    }

    #[test]
    fn a_phy_that_never_clears_busy_is_an_error_not_a_hang() {
        // The bound is the whole point. An unbounded poll against a PHY held in
        // reset is the PollUntilSet hang the board watchdog exists to catch — and
        // this is the most likely place on this device to meet it. Assert the error
        // value, not merely that the test finished.
        let mut phy = FakePhy::never_answers();
        assert_eq!(Mdio::new(4).read(&mut phy, 0, 2), Err(MdioError::Timeout));
        assert!(phy.reads.get() <= MDIO_MAX_POLLS, "the poll must be bounded");
    }

    #[test]
    fn a_write_to_an_unresponsive_phy_is_also_bounded() {
        let mut phy = FakePhy::never_answers();
        assert_eq!(Mdio::new(4).write(&mut phy, 0, 2, 1), Err(MdioError::Timeout));
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
