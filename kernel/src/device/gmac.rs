//! JH7110 GMAC reconnaissance — the read-only probe behind `workload=gmac-probe`.
//!
//! What to read, and what the words mean, is the host-tested
//! `kernel_devices::gmac`; this file is the thin `unsafe` MMIO glue that applies it.
//! Same split as [`crate::device::pwmdac`] over `kernel_devices::pwmdac`. Design:
//! `docs/vf2-gmac-design.md` ("Rung −1"); plan: `plans/vf2-gmac-driver.md`.
//!
//! **This module writes nothing.** The state it captures is what U-Boot left behind
//! after netbooting the kernel, and the driver's first reset assert destroys it — so
//! the reads cannot live inside bring-up, and a probe that perturbed what it measured
//! would be worse than no probe.
//!
//! **Board-only.** `0x1604_0000` is JH7110; under QEMU `virt` and snemu nothing
//! answers there. Unlike [`crate::device::pwmdac`] this is not address-driven and has
//! no emulated counterpart, which is why no itest scenario selects it.

use kernel_devices::gmac::{
    gmac1_resets_released, is_expected_core, snps_version, Descriptor, GmacTransport, Region,
    Target, TxError, TxRing, BASE, DMA_CH0_TX_BASE_ADDR, DMA_CH0_TX_BASE_ADDR_HI,
    DMA_CH0_TX_CONTROL, DMA_CH0_TX_END_ADDR, DMA_CH0_TX_RING_LEN, DMA_CONTROL_ST, PROBE_REGIONS,
    TDES3_ERROR_SUMMARY, TDES3_OWN,
};

/// Descriptor ring depth. Telemetry is bursty at heartbeat cadence; 8 is slack, and
/// `TxRing`'s usable capacity is one fewer. A tuning knob, not a design decision.
const TX_SLOTS: usize = 8;

/// Per-slot TX buffer. One MTU plus the Ethernet header, rounded up.
const TX_BUF_BYTES: usize = 1536;

/// The ring **and** the buffers it points at, in one `static`.
///
/// **Both must live here, not on the heap.** `mmu::va_to_pa` only translates
/// `KERNEL_OFFSET`-range addresses and passes anything else through *unchanged* —
/// so a heap-allocated ring or buffer would hand the device a kernel VA where it
/// expects a physical address, and it would DMA whatever physical memory happens to
/// sit at that number. Silently. This is the same trap that made `virtio_console`
/// stage through a static `TX_STAGING` buffer; the GMAC has two address-bearing
/// structures rather than one, so it is the same mistake available twice.
///
/// `repr(C)` with `ring` first because the device is handed this struct's address
/// as the descriptor-list base, and `TxRing`'s own `repr(C)` puts its descriptor
/// array first in turn.
#[repr(C, align(64))]
struct TxMemory {
    ring: TxRing<TX_SLOTS>,
    buffers: [[u8; TX_BUF_BYTES]; TX_SLOTS],
}

static mut TX: TxMemory =
    TxMemory { ring: TxRing::new(), buffers: [[0; TX_BUF_BYTES]; TX_SLOTS] };

/// The MAC's register file over volatile MMIO at [`BASE`].
struct Mmio;

impl GmacTransport for Mmio {
    fn read_reg(&self, offset: usize) -> u32 {
        // SAFETY: `offset` comes from `kernel_devices::gmac`'s register constants,
        // every one of which `every_target_lies_inside_its_region` proves is inside
        // the GMAC window `kmain` mapped. A 32-bit MMIO register read is naturally
        // aligned and side-effect-free on this device.
        unsafe { ((BASE + offset) as *const u32).read_volatile() }
    }

    fn write_reg(&mut self, offset: usize, value: u32) {
        // SAFETY: as above; the window is mapped writable and these offsets are
        // device registers, not RAM.
        unsafe { ((BASE + offset) as *mut u32).write_volatile(value) };
    }
}

/// Read one register and report it. The breadcrumb is emitted **before** the access,
/// never after: the failure this guards is a bus hang on a gated peripheral, which
/// produces no output at all, so a trailing breadcrumb would say nothing about where
/// the boot stopped. The last `read ...` line on the wire names the register that
/// hung.
fn read_reported(region: &Region, target: &Target) -> u32 {
    let addr = region.base + target.offset;
    crate::tracing::emit_log(&alloc::format!(
        "gmac-probe: read {}.{} @ {addr:#x}",
        region.label,
        target.label,
    ));
    // SAFETY: `addr` is inside `region`, which `every_region_is_covered_by_a_declared_
    // megapage` proves is covered by a megapage `probe` mapped above, and
    // `every_target_lies_inside_its_region` proves the offset does not run past it.
    // A `u32` MMIO register read is naturally aligned (every offset in the table is a
    // multiple of 4) and has no side effects on this device.
    let value = unsafe { (addr as *const u32).read_volatile() };
    crate::tracing::emit_log(&alloc::format!(
        "gmac-probe: {}.{} = {value:#010x}",
        region.label,
        target.label,
    ));
    value
}

/// Dump what U-Boot left configured, then stop. Nothing is written, and nothing
/// depends on the result — this is reconnaissance whose output is read by a human
/// (or the board bridge) off the UART.
/// The megapages this needs are inserted by `kmain` pre-MMU from
/// `kernel_devices::gmac::PROBE_MEGAPAGES`, alongside SYSCRG's — see the
/// `mmio_regions` block there. Nothing is mapped here.
///
/// `dtb` must still be borrowable — this runs before `kmain` tears down the
/// identity gigapage the DTB lives in.
pub fn probe(dtb: &fdt::Fdt) {
    crate::tracing::emit_log("gmac-probe: start");

    // The device tree first, deliberately: none of it can hang the bus, and its
    // answers say whether the MMIO reads below are even worth attempting.
    crate::tracing::emit_log(&alloc::format!(
        "gmac-probe: dtb dma-noncoherent={} (true means STOP — no Zicbom on this core)",
        crate::dtb::dma_is_noncoherent(dtb),
    ));
    crate::dtb::report_gmac_nodes(dtb);

    for (region_index, region) in PROBE_REGIONS.iter().enumerate() {
        for (target_index, target) in region.targets.iter().enumerate() {
            let value = read_reported(region, target);

            // The MAC's version register is first, and its verdict gates belief in
            // everything after it: a wrong base or a still-asserted reset yields
            // plausible words rather than an error. The dump continues either way —
            // the point is that it says which case it is.
            if region_index == 0 && target_index == 0 {
                crate::tracing::emit_log(&alloc::format!(
                    "gmac-probe: core version {:#04x} — {}",
                    snps_version(value),
                    if is_expected_core(value) {
                        "expected dwmac-5.20; the rest of this dump can be believed"
                    } else {
                        "NOT dwmac-5.20; base or offset is wrong, treat the rest as noise"
                    },
                ));
            }

            if target.label == "rst_gmac1_status" {
                crate::tracing::emit_log(&alloc::format!(
                    "gmac-probe: gmac1 resets {}",
                    if gmac1_resets_released(value) { "released" } else { "ASSERTED" },
                ));
            }
        }
    }

    crate::tracing::emit_log("gmac-probe: done");
}

/// `fence w, w` — order every prior store before every later one. RISC-V orders no
/// two plain stores, so this is what makes the publish sequence in [`transmit`] real
/// rather than aspirational.
fn fence_w() {
    // SAFETY: a fence has no operands and no memory effects of its own.
    unsafe { core::arch::asm!("fence w, w", options(nostack, preserves_flags)) };
}

/// Physical address of the descriptor ring — what the device is told to walk.
fn ring_pa() -> u64 {
    let va = (&raw const TX).cast::<u8>() as usize;
    crate::mmu::va_to_pa(va) as u64
}

/// Physical address of slot `slot`'s TX buffer.
fn buffer_pa(slot: usize) -> u64 {
    let base = (&raw const TX).cast::<u8>() as usize;
    let offset = core::mem::offset_of!(TxMemory, buffers) + slot * TX_BUF_BYTES;
    crate::mmu::va_to_pa(base + offset) as u64
}

/// Program the ring's base and length into the MAC and start the TX DMA engine.
fn init_tx(mmio: &mut Mmio) {
    let base = ring_pa();
    // High half first, then low — the low write is the one the device latches, so
    // the pair is never observed half-updated. Same order the fw_cfg DMA path uses.
    mmio.write_reg(DMA_CH0_TX_BASE_ADDR_HI, (base >> 32) as u32);
    mmio.write_reg(DMA_CH0_TX_BASE_ADDR, (base & 0xFFFF_FFFF) as u32);
    mmio.write_reg(DMA_CH0_TX_RING_LEN, TX_SLOTS as u32);
    mmio.write_reg(DMA_CH0_TX_CONTROL, DMA_CONTROL_ST);
}

/// Hand one complete Ethernet frame to the MAC.
///
/// The ordering is the correctness story, and none of it is expressible in the pure
/// ring: copy the body, fence, write the descriptor with `OWN` clear, fence, publish,
/// fence, kick the tail pointer. Without those fences the device can observe `OWN`
/// over a half-written descriptor, or a descriptor naming a buffer whose bytes have
/// not landed — both transmit garbage, intermittently, from a site nowhere near the
/// symptom.
pub fn transmit(frame: &[u8]) -> Result<(), TxError> {
    let len = u32::try_from(frame.len()).map_err(|_| TxError::BadLength)?;
    if frame.len() > TX_BUF_BYTES {
        return Err(TxError::BadLength);
    }

    // SAFETY: single-hart boot-path use — `tx_smoke` is the only caller and does not
    // re-enter. `&mut *(&raw mut …)` is the required idiom for a `static mut`; a
    // direct `&mut TX` is forbidden and clippy's autofix would rewrite it to one.
    #[allow(clippy::deref_addrof, reason = "the &raw mut idiom for a static mut")]
    let tx = unsafe { &mut *(&raw mut TX) };

    // The slot has to be known before the copy: the descriptor names *this* slot's
    // buffer, so the frame must already be in it.
    let slot = tx.ring.next_free_slot().ok_or(TxError::Full)?;
    tx.buffers[slot][..frame.len()].copy_from_slice(frame);
    fence_w();

    let submitted = tx.ring.submit(buffer_pa(slot), len)?;
    debug_assert_eq!(submitted, slot, "next_free_slot disagreed with submit");
    fence_w();
    tx.ring.publish(slot);
    fence_w();

    let tail = ring_pa() + (slot as u64 + 1) * core::mem::size_of::<Descriptor>() as u64;
    Mmio.write_reg(DMA_CH0_TX_END_ADDR, (tail & 0xFFFF_FFFF) as u32);
    Ok(())
}

/// Reclaim every descriptor the device has returned, stopping at the first it still
/// owns. Returns how many were freed.
///
/// The ownership read is **volatile**: the device clears `OWN` by DMA, so a plain
/// read is a read of memory the compiler believes nothing writes — it may hoist it
/// out of the loop and spin forever on a stale word.
pub fn reclaim() -> Reclaimed {
    // SAFETY: as in `transmit`.
    #[allow(clippy::deref_addrof, reason = "the &raw mut idiom for a static mut")]
    let tx = unsafe { &mut *(&raw mut TX) };

    let mut freed = 0;
    let mut failed = 0;
    while let Some(slot) = tx.ring.peek_reclaimable() {
        let tdes3_va = (&raw const TX).cast::<u8>() as usize
            + slot * core::mem::size_of::<Descriptor>()
            + core::mem::offset_of!(Descriptor, tdes3);
        // SAFETY: inside the `TX` static, naturally aligned, and volatile because
        // the device writes it behind the compiler's back.
        let tdes3 = unsafe { (tdes3_va as *const u32).read_volatile() };
        if tdes3 & TDES3_OWN != 0 {
            break;
        }
        if tdes3 & TDES3_ERROR_SUMMARY != 0 {
            // The device took the descriptor and failed the transmit — the shape a
            // wrong buffer address makes. Reported, never silently reclaimed.
            crate::tracing::emit_log(&alloc::format!(
                "gmac: transmit failed on slot {slot} (tdes3={tdes3:#010x}) — buffer PA \
                 unreachable? check va_to_pa on a non-KERNEL_OFFSET address"
            ));
            failed += 1;
        }
        tx.ring.release_one();
        freed += 1;
    }
    Reclaimed { freed, failed }
}

/// What one [`reclaim`] pass found.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Reclaimed {
    /// Descriptors the device handed back.
    pub freed: usize,
    /// How many of those reported a failed transmit.
    pub failed: usize,
}

/// The MDIO address the board's PHY sits at — `ethernet-phy@0` in the VF2 DTS.
const PHY_MDIO_ADDRESS: u8 = 0;

/// MDC divider field for the MDIO controller.
///
/// **A starting point, not a measured value** — it depends on the AHB rate, which
/// depends on what U-Boot left the clock tree in. Design-note open question 1. Too
/// fast and the PHY answers plausible garbage that reads like a wiring fault, so if
/// T1 reports an implausible id this is the first thing to sweep.
const CSR_CLOCK_RANGE: u32 = 4;

/// How many times to poll `BMSR` for link before giving up. Bounded for the same
/// reason MDIO's own poll is.
const LINK_POLLS: u32 = 100_000;

/// `workload=gmac-phy`: T1 (the PHY answers) then T2 (link up), reported separately
/// because they fail for disjoint reasons — MDC divider and MDIO pads versus RGMII
/// pads, delays and the cable.
pub fn phy_smoke() {
    use kernel_devices::phy;

    crate::tracing::emit_log("gmac-phy: start");
    let mdio = kernel_devices::gmac::Mdio::new(CSR_CLOCK_RANGE);
    let mut mmio = Mmio;

    // T1. The identity is a known constant, so a correct answer cannot come from
    // noise — which is the whole reason this rung exists before any link attempt.
    let id1 = mdio.read(&mut mmio, PHY_MDIO_ADDRESS, phy::PHY_ID1);
    let id2 = mdio.read(&mut mmio, PHY_MDIO_ADDRESS, phy::PHY_ID2);
    let (Ok(id1), Ok(id2)) = (id1, id2) else {
        crate::tracing::emit_log(
            "gmac-phy: MDIO never cleared GBUSY — the PHY is held in reset, absent, \
             or the MDC divider (CSR_CLOCK_RANGE) is wrong for this AHB rate",
        );
        return;
    };
    let id = phy::phy_id(id1, id2);
    if phy::is_yt853x(id) {
        crate::tracing::emit_log(&alloc::format!("gmac-phy: phy id {id:#010x} — a YT853x"));
    } else {
        crate::tracing::emit_log(&alloc::format!(
            "gmac-phy: phy id {id:#010x} — NOT a YT853x. 0xffffffff is a floating bus \
             (wrong PHY address or MDIO pads unrouted); 0x00000000 is a PHY held in reset"
        ));
        return;
    }

    // T2. Advertise only what we intend to drive, then restart negotiation — both
    // bits, because RESTART without ENABLE is a silent no-op.
    if mdio.write(&mut mmio, PHY_MDIO_ADDRESS, phy::ANAR, phy::advertise_100_full()).is_err()
        || mdio.write(&mut mmio, PHY_MDIO_ADDRESS, phy::BMCR, phy::restart_autoneg()).is_err()
    {
        crate::tracing::emit_log("gmac-phy: MDIO write timed out configuring the PHY");
        return;
    }

    let mut polls = 0;
    let up = loop {
        if polls >= LINK_POLLS {
            break false;
        }
        polls += 1;
        // Two reads, because BMSR's link bit is latch-low — a single read of a link
        // that blipped reports down. `link_is_up` takes a reader so it cannot be
        // called with one.
        let mut failed = false;
        let up = phy::link_is_up(|| {
            mdio.read(&mut mmio, PHY_MDIO_ADDRESS, phy::BMSR).unwrap_or_else(|_| {
                failed = true;
                0
            })
        });
        if failed {
            crate::tracing::emit_log("gmac-phy: MDIO stopped answering while polling link");
            return;
        }
        if up {
            break true;
        }
    };

    if up {
        crate::tracing::emit_log(&alloc::format!("gmac-phy: link up after {polls} polls"));
    } else {
        crate::tracing::emit_log(
            "gmac-phy: link never came up — cable, PHY reset GPIO, RGMII pads, or the \
             delay/TX_INV clock selection. The PHY answered, so MDIO itself is fine",
        );
    }
    crate::tracing::emit_log("gmac-phy: done");
}

/// Build one real UDP datagram with `kernel-net` and push it through the
/// [`NetDevice`] impl — the production path, not a hand-built frame.
///
/// Deliberately **after** the raw-frame submission above rather than instead of it.
/// The raw frame asks "did the engine take a descriptor"; this asks "does the whole
/// stack produce something the engine takes". Merging them would give a single
/// failure three layers of suspects, which is the exact mistake the ladder exists to
/// avoid.
///
/// [`NetDevice`]: kernel_net::NetDevice
fn send_one_datagram() {
    use kernel_net::NetDevice;

    // A self-contained config: broadcast destination so nothing needs ARP, and a
    // locally-administered source MAC. The real `net=` path takes these from the
    // bootarg; this workload is a transmit smoke, not a configuration test.
    let config = kernel_net::NetConfig {
        src_mac: [0x02, 0x00, 0x00, 0x00, 0x00, 0x01],
        src_ip: [10, 0, 0, 2],
        dst_mac: [0xFF; 6],
        dst_ip: [10, 0, 0, 1],
        src_port: 5001,
        dst_port: 5001,
    };
    let mut buf = [0u8; 128];
    let datagram = match kernel_net::build_udp_datagram(&config, b"gmac-tx probe", &mut buf) {
        Ok(d) => d,
        Err(_) => {
            crate::tracing::emit_log("gmac-tx: datagram did not fit its buffer");
            return;
        }
    };
    let len = datagram.len();
    match Gmac.send(datagram) {
        Ok(()) => crate::tracing::emit_log(&alloc::format!(
            "gmac-tx: submitted a {len}-byte udp datagram"
        )),
        Err(_) => crate::tracing::emit_log("gmac-tx: ring full for the udp datagram"),
    }

    let result = reclaim_until_idle();
    if result.freed > 0 && result.failed == 0 {
        crate::tracing::emit_log("gmac-tx: engine transmitted the udp datagram");
    } else {
        crate::tracing::emit_log("gmac-tx: udp datagram was not transmitted");
    }
}

/// Poll [`reclaim`] until it frees something or the bound runs out. Bounded for the
/// same reason MDIO's poll is: an engine that never answers must say so.
fn reclaim_until_idle() -> Reclaimed {
    let mut result = Reclaimed::default();
    for _ in 0..100_000u32 {
        result = reclaim();
        if result.freed > 0 {
            break;
        }
    }
    result
}

/// Bridges the UDP batcher to this driver. Unlike `virtio_net`'s `send`, which
/// spins to completion and so can report `Ok` unconditionally, a descriptor ring
/// genuinely runs out of slots — so [`TxFull`] here is real back-pressure, not a
/// formality. The caller drops the frame and counts it, which is the right trade for
/// fire-and-forget telemetry: blocking the emitter to save one frame would be worse
/// than losing it.
///
/// [`TxFull`]: kernel_net::TxFull
pub struct Gmac;

impl kernel_net::NetDevice for Gmac {
    fn send(&mut self, frame: &[u8]) -> Result<(), kernel_net::TxFull> {
        // Reclaim first: without it the ring fills after `TX_SLOTS - 1` frames and
        // never drains, because nothing else polls. Cheap — it stops at the first
        // descriptor the device still owns.
        reclaim();
        transmit(frame).map_err(|_| kernel_net::TxFull)
    }
}

/// `workload=gmac-tx`: program the ring, hand the MAC one frame, report whether the
/// engine took it. T3 and T4 of the design note's ladder — against snemu's model at
/// the desk, against silicon on the board.
pub fn tx_smoke() {
    crate::tracing::emit_log("gmac-tx: start");
    init_tx(&mut Mmio);

    // Broadcast so no switch can drop it, custom ethertype so nothing else claims
    // it. The payload is deliberately **not** a valid IP packet: T3 asks whether the
    // engine *took* the descriptor, and conflating that with "is the frame
    // well-formed" is what gives a silent tcpdump a dozen causes instead of three.
    let mut frame = [0u8; 64];
    frame[..6].copy_from_slice(&[0xFF; 6]);
    frame[6..12].copy_from_slice(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);
    frame[12..14].copy_from_slice(&0x88B5u16.to_be_bytes());
    frame[14..18].copy_from_slice(b"SNCH");

    if let Err(e) = transmit(&frame) {
        crate::tracing::emit_log(&alloc::format!("gmac-tx: submit refused: {e:?}"));
        return;
    }
    crate::tracing::emit_log("gmac-tx: submitted 1 frame");

    // Bounded for the same reason MDIO's poll is: an engine that never answers must
    // say so rather than wedge the boot.
    let mut result = Reclaimed::default();
    for _ in 0..100_000u32 {
        result = reclaim();
        if result.freed > 0 {
            break;
        }
    }

    if result.freed > 0 && result.failed == 0 {
        crate::tracing::emit_log("gmac-tx: engine transmitted the frame");
        send_one_datagram();
    } else if result.failed > 0 {
        crate::tracing::emit_log("gmac-tx: engine returned the descriptor but reported failure");
    } else {
        crate::tracing::emit_log(
            "gmac-tx: engine never cleared OWN — descriptor PA wrong (va_to_pa on a \
             non-KERNEL_OFFSET address?), ring base not programmed, or ST not set",
        );
    }
    crate::tracing::emit_log("gmac-tx: done");
}
