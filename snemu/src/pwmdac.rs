//! snemu's model of the JH7110 PWMDAC (+ its SYSCRG clock/reset window) — a
//! synthetic device added to snemu's virt machine so the `audio-beep` workload can
//! run off-hardware (see `plans/vf2-audio-tier0.md` Increment 9a, fork (a)). It
//! captures the `WDATA` sample stream for `--audio-out` to render (via `audio`),
//! and — crucially — makes the guest's clock/reset bring-up *complete*: without a
//! device here, a guest write to `0x100b0000` falls through to RAM and halts the
//! run (`bus.rs`).
//!
//! Two register windows:
//! - **PWMDAC** `0x100b0000` — `WDATA` (+0x00, sample port, captured) and `CTRL`
//!   (+0x04, captured). Reads return 0 (the guest only writes these; its `CTRL`
//!   read-modify-write starts from 0, which snemu then captures on write-back).
//! - **SYSCRG** `0x13020000` — clock gates + reset registers. snemu models no real
//!   clock tree, so reads return all-ones: every clock reads enabled and every
//!   reset reads *released*, which is what lets the guest's reset-status
//!   `PollUntilSet` (Increment 3) complete instead of spinning. Writes are swallowed.

const PWMDAC_BASE: u64 = 0x100b_0000;
const PWMDAC_SIZE: u64 = 0x1000;
const WDATA_OFFSET: u64 = 0x00;
const CTRL_OFFSET: u64 = 0x04;

const SYSCRG_BASE: u64 = 0x1302_0000;
const SYSCRG_SIZE: u64 = 0x1_0000;

/// SYS_IOMUX (pin-mux) — the guest routes the PWMDAC output pads here. snemu has no
/// pads, so it just swallows the writes (reads fall through to 0); the point is only
/// that the guest doesn't halt on an unmapped write.
const IOMUX_BASE: u64 = 0x1304_0000;
const IOMUX_SIZE: u64 = 0x1_0000;

#[derive(Clone)]
pub(crate) struct Pwmdac {
    samples: Vec<i16>,
    ctrl: u32,
}

impl Pwmdac {
    pub(crate) fn new() -> Self {
        Self { samples: Vec::new(), ctrl: 0 }
    }

    /// Whether `addr` is in the PWMDAC or the SYSCRG register window.
    pub(crate) fn in_window(addr: u64) -> bool {
        (PWMDAC_BASE..PWMDAC_BASE + PWMDAC_SIZE).contains(&addr)
            || (SYSCRG_BASE..SYSCRG_BASE + SYSCRG_SIZE).contains(&addr)
            || (IOMUX_BASE..IOMUX_BASE + IOMUX_SIZE).contains(&addr)
    }

    /// Read semantics for the two windows: 0 for the PWMDAC registers,
    /// all-ones for SYSCRG (all clocks enabled / resets released — enough for the
    /// guest bring-up's `PollUntilSet` to complete). Depends only on `addr`, not on
    /// captured state.
    pub(crate) fn read(addr: u64) -> u64 {
        if (SYSCRG_BASE..SYSCRG_BASE + SYSCRG_SIZE).contains(&addr) {
            return u64::MAX;
        }
        0
    }

    /// Route a write: capture a sample on `WDATA`, the control word on `CTRL`,
    /// swallow every other PWMDAC/SYSCRG offset. `value` is truncated to the
    /// 16-bit PCM sample the guest wrote.
    pub(crate) fn write(&mut self, addr: u64, value: u32) {
        // SYSCRG and other non-PWMDAC addresses return here. (Mutation testing
        // flags `PWMDAC_BASE + PWMDAC_SIZE → *` as a survivor: it's equivalent —
        // widening the upper bound only admits high offsets, which the match below
        // swallows via `_ => {}`, so behaviour is unchanged.)
        if !(PWMDAC_BASE..PWMDAC_BASE + PWMDAC_SIZE).contains(&addr) {
            return;
        }
        match addr - PWMDAC_BASE {
            WDATA_OFFSET => self.samples.push(value as i16),
            CTRL_OFFSET => self.ctrl = value,
            _ => {}
        }
    }

    /// The captured signed-PCM sample stream, in write order.
    pub(crate) fn samples(&self) -> &[i16] {
        &self.samples
    }

    /// Fold the captured stream into the machine-state hash (determinism): the
    /// samples and the control word are the device's own semantic state.
    pub(crate) fn hash_state(&self, h: &mut impl std::hash::Hasher) {
        use std::hash::Hash;
        self.samples.hash(h);
        self.ctrl.hash(h);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mem::RAM_BASE;

    #[test]
    fn wdata_writes_accumulate_as_signed_pcm_samples() {
        let mut dac = Pwmdac::new();
        dac.write(PWMDAC_BASE + WDATA_OFFSET, 100);
        dac.write(PWMDAC_BASE + WDATA_OFFSET, 0xFFFF); // low 16 bits = -1
        assert_eq!(dac.samples(), &[100, -1]);
    }

    #[test]
    fn ctrl_write_is_folded_into_state_and_is_not_a_sample() {
        let mut dac = Pwmdac::new();
        dac.write(PWMDAC_BASE + CTRL_OFFSET, 0x4019);
        assert!(dac.samples().is_empty(), "CTRL is not a sample");
        // Captured as device state: it moves the hash away from an untouched device.
        assert_ne!(hash_of(&dac), hash_of(&Pwmdac::new()), "CTRL is folded into the hash");
    }

    #[test]
    fn unmodeled_offsets_are_swallowed() {
        let mut dac = Pwmdac::new();
        dac.write(PWMDAC_BASE + 0x08, 0xDEAD); // un-modeled PWMDAC offset
        dac.write(SYSCRG_BASE + 0x304, 0); // a SYSCRG reset write
        assert!(dac.samples().is_empty());
        assert_eq!(hash_of(&dac), hash_of(&Pwmdac::new()), "state untouched");
    }

    #[test]
    fn in_window_covers_both_regions_but_not_ram() {
        assert!(Pwmdac::in_window(PWMDAC_BASE));
        assert!(Pwmdac::in_window(PWMDAC_BASE + CTRL_OFFSET));
        assert!(Pwmdac::in_window(SYSCRG_BASE + 0x314));
        assert!(Pwmdac::in_window(IOMUX_BASE), "pin-mux writes must be accepted, not halt");
        assert!(!Pwmdac::in_window(RAM_BASE));
        assert!(!Pwmdac::in_window(0));
        assert!(!Pwmdac::in_window(PWMDAC_BASE + PWMDAC_SIZE), "just past the window");
    }

    #[test]
    fn pwmdac_reads_zero_and_syscrg_reads_all_ones() {
        assert_eq!(Pwmdac::read(PWMDAC_BASE), 0);
        assert_eq!(Pwmdac::read(PWMDAC_BASE + CTRL_OFFSET), 0);
        // All-ones so the guest's reset-status `PollUntilSet` completes.
        assert_eq!(Pwmdac::read(SYSCRG_BASE + 0x314), u64::MAX);
        // Just past the SYSCRG window is not all-ones — pins the upper bound.
        assert_eq!(Pwmdac::read(SYSCRG_BASE + SYSCRG_SIZE), 0);
    }

    fn hash_of(dac: &Pwmdac) -> u64 {
        use std::hash::Hasher;
        let mut h = std::collections::hash_map::DefaultHasher::new();
        dac.hash_state(&mut h);
        h.finish()
    }

    #[test]
    fn identical_capture_hashes_equal_but_different_samples_differ() {
        let mut a = Pwmdac::new();
        a.write(PWMDAC_BASE, 7);
        a.write(PWMDAC_BASE, 8);
        let mut b = Pwmdac::new();
        b.write(PWMDAC_BASE, 7);
        b.write(PWMDAC_BASE, 8);
        assert_eq!(hash_of(&a), hash_of(&b));

        let mut c = Pwmdac::new();
        c.write(PWMDAC_BASE, 7);
        c.write(PWMDAC_BASE, 9);
        assert_ne!(hash_of(&a), hash_of(&c), "the captured stream must affect the hash");
    }
}
