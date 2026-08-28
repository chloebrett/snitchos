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

/// `DMA_CHAN_TX_BASE_ADDR` (channel 0), low half.
const TX_BASE_ADDR: u64 = 0x1114;
/// `DMA_CHAN_TX_BASE_ADDR_HI` (channel 0).
const TX_BASE_ADDR_HI: u64 = 0x1110;
/// `DMA_CHAN_TX_RING_LEN` (channel 0).
const TX_RING_LEN: u64 = 0x112C;
/// `DMA_CHAN_TX_END_ADDR` — the tail pointer. Writing it is the **transmit
/// trigger**, matching the real device and the guest driver's write order.
const TX_END_ADDR: u64 = 0x1120;

/// `TDES3_OWN`.
const TDES3_OWN: u32 = 1 << 31;
/// `TDES3_ERROR_SUMMARY`, in the writeback format — the device's way of saying the
/// transmit failed.
const TDES3_ERROR_SUMMARY: u32 = 1 << 15;
/// `TDES2_BUFFER1_SIZE_MASK`.
const TDES2_SIZE_MASK: u32 = (1 << 14) - 1;
/// One descriptor is four 32-bit words.
const DESCRIPTOR_BYTES: u64 = 16;
/// How many descriptors a single kick will walk, whatever the ring length register
/// says — a runaway guard, since a corrupt `TX_RING_LEN` would otherwise make the
/// model walk guest RAM forever.
const MAX_WALK: u32 = 64;

/// The modelled GMAC1: enough register state to accept a TX descriptor ring, and
/// the frames it has "transmitted".
#[derive(Clone, Default)]
pub(crate) struct Gmac {
    tx_base: u64,
    tx_ring_len: u32,
    /// Where the engine is in the ring. **Not reset per kick**: the device resumes
    /// from its current descriptor, it does not rescan from the base. Restarting at
    /// slot 0 would make the first transmit work and every one after it stop dead on
    /// the already-returned slot 0 — a failure that looks like a guest bug.
    tx_current: u32,
    frames: Vec<Vec<u8>>,
}

impl Gmac {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Whether `addr` is in the GMAC1 or `sys_syscon` window.
    pub(crate) fn in_window(addr: u64) -> bool {
        (GMAC1_BASE..GMAC1_BASE + GMAC1_SIZE).contains(&addr)
            || (SYS_SYSCON_BASE..SYS_SYSCON_BASE + SYS_SYSCON_SIZE).contains(&addr)
    }

    /// Whether writing `addr` hands the device control — the bus calls
    /// [`service_tx`] after such a write, the same split `Virtio::is_notify` uses.
    ///
    /// [`service_tx`]: Gmac::service_tx
    pub(crate) fn is_tx_kick(addr: u64) -> bool {
        addr == GMAC1_BASE + TX_END_ADDR
    }

    /// Frames handed to the device so far, oldest first.
    pub(crate) fn frames(&self) -> &[Vec<u8>] {
        &self.frames
    }

    /// Read semantics: the version register identifies the core, the DMA registers
    /// read back what the guest wrote, everything else is zero — what a MAC nobody
    /// has configured looks like.
    pub(crate) fn read(&self, addr: u64) -> u64 {
        match addr {
            a if a == GMAC1_BASE + VERSION_OFFSET => SNPSVER_5_20,
            a if a == SYS_SYSCON_BASE + 0x90 => PHY_INTF_SEL_WORD,
            a if a == GMAC1_BASE + TX_BASE_ADDR => self.tx_base & 0xFFFF_FFFF,
            a if a == GMAC1_BASE + TX_BASE_ADDR_HI => self.tx_base >> 32,
            a if a == GMAC1_BASE + TX_RING_LEN => u64::from(self.tx_ring_len),
            _ => 0,
        }
    }

    /// Capture the DMA registers; swallow everything else.
    pub(crate) fn write(&mut self, addr: u64, value: u32) {
        let value64 = u64::from(value);
        match addr {
            a if a == GMAC1_BASE + TX_BASE_ADDR => {
                self.tx_base = (self.tx_base & 0xFFFF_FFFF_0000_0000) | value64;
            }
            a if a == GMAC1_BASE + TX_BASE_ADDR_HI => {
                self.tx_base = (self.tx_base & 0xFFFF_FFFF) | (value64 << 32);
            }
            a if a == GMAC1_BASE + TX_RING_LEN => self.tx_ring_len = value,
            _ => {}
        }
    }

    /// Walk the descriptor ring from `tx_base`, transmitting every descriptor the
    /// guest owns us and handing each back by clearing `OWN`. Stops at the first
    /// descriptor software still owns — completion is in order.
    pub(crate) fn service_tx(&mut self, ram: &mut crate::mem::Memory) {
        if self.tx_base == 0 {
            return;
        }
        if self.tx_ring_len == 0 {
            return;
        }
        for _ in 0..self.tx_ring_len.min(MAX_WALK) {
            let slot = self.tx_current;
            let desc = self.tx_base + u64::from(slot) * DESCRIPTOR_BYTES;
            let tdes3 = ram.read_u32(desc + 12).unwrap_or(0);
            if tdes3 & TDES3_OWN == 0 {
                return;
            }
            let lo = u64::from(ram.read_u32(desc).unwrap_or(0));
            let hi = u64::from(ram.read_u32(desc + 4).unwrap_or(0));
            let len = ram.read_u32(desc + 8).unwrap_or(0) & TDES2_SIZE_MASK;
            let buf = (hi << 32) | lo;
            let fetched: Option<Vec<u8>> =
                (0..u64::from(len)).map(|i| ram.read_u8(buf + i).ok()).collect();

            // A descriptor whose buffer address does not resolve is a failed DMA
            // fetch, and real silicon reports that in the writeback's error-summary
            // bit rather than transmitting silently. Modelling it as success would
            // make `OWN` cleared mean "the buffer address was fine", which it does
            // not — the whole reason a bad `va_to_pa` is a *silent* bug.
            let writeback = match fetched {
                Some(frame) => {
                    self.frames.push(frame);
                    tdes3 & !TDES3_OWN
                }
                None => (tdes3 & !TDES3_OWN) | TDES3_ERROR_SUMMARY,
            };
            let _ = ram.write_u32(desc + 12, writeback);
            self.tx_current = (self.tx_current + 1) % self.tx_ring_len;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mem::Memory;

    const RAM_BASE: u64 = 0x8000_0000;

    /// Lay a one-descriptor ring at `ring_pa` pointing at `payload`, owned by the
    /// device — the shape `kernel_devices::gmac::Descriptor::prepare` produces.
    fn rig(mem: &mut Memory, ring_pa: u64, buf_pa: u64, payload: &[u8]) {
        for (i, &b) in payload.iter().enumerate() {
            mem.write_u8(buf_pa + i as u64, b).unwrap();
        }
        let len = u32::try_from(payload.len()).unwrap();
        mem.write_u32(ring_pa, (buf_pa & 0xFFFF_FFFF) as u32).unwrap();
        mem.write_u32(ring_pa + 4, (buf_pa >> 32) as u32).unwrap();
        mem.write_u32(ring_pa + 8, len).unwrap();
        // OWN | FD | LD | packet length
        mem.write_u32(ring_pa + 12, 0x8000_0000 | 0x2000_0000 | 0x1000_0000 | len).unwrap();
    }

    fn armed(ring_pa: u64) -> Gmac {
        let mut g = Gmac::new();
        g.write(GMAC1_BASE + TX_BASE_ADDR_HI, (ring_pa >> 32) as u32);
        g.write(GMAC1_BASE + TX_BASE_ADDR, (ring_pa & 0xFFFF_FFFF) as u32);
        g.write(GMAC1_BASE + TX_RING_LEN, 4);
        g
    }

    #[test]
    fn a_kick_clears_ownership_on_the_descriptor() {
        // T3's assertion, at desk speed: the engine read the descriptor and gave it
        // back. That alone validates the whole address-translation story.
        let (ring_pa, buf_pa) = (RAM_BASE + 0x1000, RAM_BASE + 0x2000);
        let mut mem = Memory::new(0x10000);
        rig(&mut mem, ring_pa, buf_pa, b"hello");
        let mut g = armed(ring_pa);
        g.service_tx(&mut mem);
        assert_eq!(mem.read_u32(ring_pa + 12).unwrap() & 0x8000_0000, 0, "OWN cleared");
    }

    #[test]
    fn a_kick_captures_the_frame_the_descriptor_points_at() {
        // T4's oracle: well-formed bytes reached the device.
        let (ring_pa, buf_pa) = (RAM_BASE + 0x1000, RAM_BASE + 0x2000);
        let mut mem = Memory::new(0x10000);
        rig(&mut mem, ring_pa, buf_pa, b"hello");
        let mut g = armed(ring_pa);
        g.service_tx(&mut mem);
        assert_eq!(g.frames(), &[b"hello".to_vec()]);
    }

    #[test]
    fn a_descriptor_naming_an_unreachable_buffer_reports_an_error() {
        // The bug this exists to catch: `va_to_pa` passing a non-KERNEL_OFFSET
        // address through unchanged, so the descriptor names an address that is not
        // the frame. Clearing OWN alone would report that as success — it only ever
        // proved the *descriptor* was fetched, never the buffer.
        let ring_pa = RAM_BASE + 0x1000;
        let mut mem = Memory::new(0x10000);
        rig(&mut mem, ring_pa, RAM_BASE + 0x2000, b"hello");
        mem.write_u32(ring_pa, 0xDEAD_0000u32).unwrap(); // buffer address nowhere
        let mut g = armed(ring_pa);
        g.service_tx(&mut mem);
        let writeback = mem.read_u32(ring_pa + 12).unwrap();
        assert_eq!(writeback & 0x8000_0000, 0, "still handed back");
        assert_ne!(writeback & 0x8000, 0, "but flagged as an error");
        assert!(g.frames().is_empty(), "and nothing was transmitted");
    }

    #[test]
    fn a_successful_transmit_leaves_the_error_bit_clear() {
        let (ring_pa, buf_pa) = (RAM_BASE + 0x1000, RAM_BASE + 0x2000);
        let mut mem = Memory::new(0x10000);
        rig(&mut mem, ring_pa, buf_pa, b"hello");
        let mut g = armed(ring_pa);
        g.service_tx(&mut mem);
        assert_eq!(mem.read_u32(ring_pa + 12).unwrap() & 0x8000, 0);
    }

    #[test]
    fn successive_kicks_resume_where_the_last_one_stopped() {
        // The device keeps a current-descriptor cursor; it does not rescan from the
        // ring base. Restarting at slot 0 makes the *second* transmit stop dead —
        // slot 0's OWN is already clear — so one frame works and every frame after
        // it silently doesn't. Exactly the shape that looks like a guest bug.
        let (ring_pa, buf_a, buf_b) = (RAM_BASE + 0x1000, RAM_BASE + 0x2000, RAM_BASE + 0x3000);
        let mut mem = Memory::new(0x10000);
        let mut g = armed(ring_pa);

        rig(&mut mem, ring_pa, buf_a, b"first");
        g.service_tx(&mut mem);

        rig(&mut mem, ring_pa + 16, buf_b, b"second");
        g.service_tx(&mut mem);

        assert_eq!(g.frames(), &[b"first".to_vec(), b"second".to_vec()]);
        assert_eq!(mem.read_u32(ring_pa + 16 + 12).unwrap() & 0x8000_0000, 0, "slot 1 returned");
    }

    #[test]
    fn the_cursor_wraps_at_the_end_of_the_ring() {
        let ring_pa = RAM_BASE + 0x1000;
        let mut mem = Memory::new(0x10000);
        let mut g = armed(ring_pa);
        // Ring length is 4; fill and drain it five times to cross the wrap.
        for round in 0..5u64 {
            let slot = round % 4;
            rig(&mut mem, ring_pa + slot * 16, RAM_BASE + 0x2000, b"x");
            g.service_tx(&mut mem);
        }
        assert_eq!(g.frames().len(), 5, "the fifth kick wrapped back to slot 0");
    }

    #[test]
    fn a_descriptor_the_guest_still_owns_is_left_alone() {
        // Software owns it until it sets OWN. Transmitting one the guest has not
        // published would send a half-written frame.
        let (ring_pa, buf_pa) = (RAM_BASE + 0x1000, RAM_BASE + 0x2000);
        let mut mem = Memory::new(0x10000);
        rig(&mut mem, ring_pa, buf_pa, b"hello");
        mem.write_u32(ring_pa + 12, 0x3000_0005).unwrap(); // FD | LD | len, OWN clear
        let mut g = armed(ring_pa);
        g.service_tx(&mut mem);
        assert!(g.frames().is_empty());
    }

    #[test]
    fn a_kick_before_the_ring_base_is_programmed_transmits_nothing() {
        let mut mem = Memory::new(0x10000);
        let mut g = Gmac::new();
        g.service_tx(&mut mem);
        assert!(g.frames().is_empty(), "no ring base — nothing to walk");
    }

    #[test]
    fn the_ring_base_register_reads_back_what_was_written() {
        // The probe reports this register; a model that always answered zero would
        // make "U-Boot left no ring" indistinguishable from "we never wrote one".
        let mut g = armed(RAM_BASE + 0x1000);
        assert_eq!(g.read(GMAC1_BASE + TX_BASE_ADDR), (RAM_BASE + 0x1000) & 0xFFFF_FFFF);
        g.write(GMAC1_BASE + TX_RING_LEN, 7);
        assert_eq!(g.read(GMAC1_BASE + TX_RING_LEN), 7);
    }

    #[test]
    fn the_version_register_identifies_a_dwmac_5_20() {
        // The low byte is what `kernel_devices::gmac::is_expected_core` checks.
        assert_eq!(Gmac::new().read(GMAC1_BASE + VERSION_OFFSET) & 0xFF, 0x52);
    }

    #[test]
    fn the_version_word_carries_a_vendor_byte_above_the_core_id() {
        // So a guest that forgot to mask fails here rather than on the board.
        assert_ne!(Gmac::new().read(GMAC1_BASE + VERSION_OFFSET), 0x52);
    }

    #[test]
    fn an_unconfigured_register_reads_zero() {
        // A MAC U-Boot never touched. Notably this is *not* all-ones: the guest's
        // poison-word check treats 0xFFFF_FFFF as a floating bus, and a model that
        // returned it would make every probe run look like a hardware fault.
        let g = Gmac::new();
        assert_eq!(g.read(GMAC1_BASE), 0);
        assert_eq!(g.read(GMAC1_BASE + TX_BASE_ADDR), 0);
    }

    #[test]
    fn the_phy_interface_field_is_non_zero() {
        assert_ne!(Gmac::new().read(SYS_SYSCON_BASE + 0x90), 0);
    }

    #[test]
    fn both_windows_are_claimed_and_neighbours_are_not() {
        assert!(Gmac::in_window(GMAC1_BASE));
        assert!(Gmac::in_window(GMAC1_BASE + GMAC1_SIZE - 1));
        assert!(Gmac::in_window(SYS_SYSCON_BASE));
        assert!(Gmac::in_window(SYS_SYSCON_BASE + SYS_SYSCON_SIZE - 1));
        assert!(!Gmac::in_window(GMAC1_BASE - 1), "GMAC0's window is not modelled");
        assert!(!Gmac::in_window(GMAC1_BASE + GMAC1_SIZE));
        assert!(!Gmac::in_window(SYS_SYSCON_BASE + SYS_SYSCON_SIZE));
    }

    #[test]
    fn only_the_tail_pointer_write_hands_the_device_control() {
        assert!(Gmac::is_tx_kick(GMAC1_BASE + TX_END_ADDR));
        assert!(!Gmac::is_tx_kick(GMAC1_BASE + TX_BASE_ADDR));
        assert!(!Gmac::is_tx_kick(GMAC1_BASE + TX_RING_LEN));
    }
}
