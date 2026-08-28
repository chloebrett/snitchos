//! IEEE 802.3 clause-22 PHY register logic — no MMIO, no MDIO bus access.
//!
//! Sits above [`crate::gmac::Mdio`], which does the bus transactions; this decides
//! *what* to read and *what the answers mean*. A sibling module rather than a child
//! of `gmac` because a PHY is a separate chip on a separate bus — the MAC is only
//! how we reach it.
//!
//! **Everything here is the standard clause-22 register set**, identical across every
//! conforming PHY. The Motorcomm YT8531's *vendor-specific* registers — the RGMII
//! delay configuration and its reset GPIO — are deliberately **not** modelled: they
//! are the design note's open question 2, "the genuinely unknown-shaped part", and
//! writing them from a datasheet reading nobody has checked would be guessing in a
//! file that looks authoritative. See `docs/vf2-gmac-design.md`.
//!
//! **On the surviving mutants here.** Every `|` in this module combines bitfields
//! that cannot overlap — the two halves of a 32-bit id, or single bits at fixed
//! standard positions — so `|` and `^` agree and mutation testing reports each as a
//! survivor. They are genuine equivalent mutants, the same class as
//! `syscrg::pwmdac_bringup`. What is *not* equivalent, and so is pinned by
//! `the_bit_constants_sit_where_the_standard_puts_them`, is the shifts that build
//! those constants: a mask that mutates to zero makes every `x & MASK == 0`
//! assertion in this file pass vacuously.

/// Basic Mode Control Register.
pub const BMCR: u8 = 0;
/// Basic Mode Status Register.
pub const BMSR: u8 = 1;
/// PHY Identifier 1 — the high 16 bits of the 32-bit id.
pub const PHY_ID1: u8 = 2;
/// PHY Identifier 2 — the low 16 bits.
pub const PHY_ID2: u8 = 3;
/// Auto-Negotiation Advertisement Register.
pub const ANAR: u8 = 4;

/// `BMCR` software reset. Self-clearing — poll it, bounded.
pub const BMCR_RESET: u16 = 1 << 15;
/// `BMCR` auto-negotiation enable.
pub const BMCR_ANEG_ENABLE: u16 = 1 << 12;
/// `BMCR` restart auto-negotiation.
pub const BMCR_ANEG_RESTART: u16 = 1 << 9;

/// `BMSR` link status. **Latch-low** — see [`link_is_up`].
pub const BMSR_LINK_UP: u16 = 1 << 2;
/// `BMSR` auto-negotiation complete.
pub const BMSR_ANEG_COMPLETE: u16 = 1 << 5;

/// `ANAR` 100BASE-TX full duplex.
pub const ANAR_100_FULL: u16 = 1 << 8;
/// `ANAR` 100BASE-TX half duplex.
pub const ANAR_100_HALF: u16 = 1 << 7;
/// `ANAR` protocol-selector field.
pub const ANAR_SELECTOR_MASK: u16 = 0x1F;
/// `ANAR` selector value for IEEE 802.3.
pub const ANAR_SELECTOR_802_3: u16 = 0x01;

/// Motorcomm YT8531 (`PHY_ID_YT8531`, mainline `motorcomm.c`).
pub const YT8531: u32 = 0x4F51_E91B;
/// Motorcomm YT8531S (`PHY_ID_YT8531S`) — the same model, different revision
/// nibble. The board's own note says only "YT8531", so both are accepted.
pub const YT8531S: u32 = 0x4F51_E91A;
/// Vendor + model bits, revision masked off — Linux's `PHY_ID_MATCH_MODEL`.
pub const MODEL_MASK: u32 = 0xFFFF_FFF0;

/// Join the two identifier registers into the 32-bit PHY id, ID1 high.
#[must_use]
pub fn phy_id(id1: u16, id2: u16) -> u32 {
    (u32::from(id1) << 16) | u32::from(id2)
}

/// Whether `id` is a Motorcomm YT853x.
///
/// **This is T1's oracle**, and its value is that the answer cannot arise from
/// noise: a floating MDIO bus reads all-ones and a PHY held in reset reads zero,
/// and neither is a valid id. Matched at *model* level so a revision difference
/// does not fail the check, but not at *vendor* level — a YT8821 is the same
/// vendor and a part this driver cannot drive.
#[must_use]
pub fn is_yt853x(id: u32) -> bool {
    id & MODEL_MASK == YT8531 & MODEL_MASK
}

/// Whether the link is up. **Reads `BMSR` twice**, via `read_bmsr`.
///
/// `BMSR_LINK_UP` is latch-low: it reports a link that has dropped *since the last
/// read*, so a single read of a healthy link that blipped earlier returns zero. The
/// second read carries the current state. A single-read implementation reports a
/// live link as down, and the search for the cause goes to cables and RGMII delays
/// rather than to this line.
///
/// It takes a reader rather than two values so the double read is **structural**: a
/// caller cannot accidentally pass the same read twice, or forget the first. Same
/// reasoning as [`Descriptor::prepare`] refusing to set the ownership bit.
///
/// [`Descriptor::prepare`]: crate::gmac::Descriptor::prepare
pub fn link_is_up(mut read_bmsr: impl FnMut() -> u16) -> bool {
    let _discarded = read_bmsr();
    read_bmsr() & BMSR_LINK_UP != 0
}

/// Whether negotiation has finished *and* produced a link. Both bits, because
/// either alone is a state you must keep waiting through.
#[must_use]
pub fn negotiation_complete(bmsr: u16) -> bool {
    bmsr & (BMSR_ANEG_COMPLETE | BMSR_LINK_UP) == BMSR_ANEG_COMPLETE | BMSR_LINK_UP
}

/// The `ANAR` value advertising 100BASE-TX full duplex and nothing else.
///
/// Gigabit is deliberately not offered: forcing 100 removes the 125 MHz GTXCLK
/// path, the gigabit RGMII timing margin, and the speed-change re-clocking — three
/// of the nastiest bring-up variables — for a link that only ever carries
/// telemetry. Take gigabit later, as its own observable step.
#[must_use]
pub fn advertise_100_full() -> u16 {
    ANAR_SELECTOR_802_3 | ANAR_100_FULL
}

/// The `BMCR` value that restarts auto-negotiation. Both bits: `RESTART` without
/// `ENABLE` is a no-op on most PHYs, and a silent one — negotiation simply never
/// completes and the link never comes up.
#[must_use]
pub fn restart_autoneg() -> u16 {
    BMCR_ANEG_ENABLE | BMCR_ANEG_RESTART
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_phy_id_is_two_registers_joined_high_first() {
        assert_eq!(phy_id(0x4F51, 0xE91B), 0x4F51_E91B);
    }

    #[test]
    fn both_yt8531_variants_match_at_model_level() {
        // The board note says "YT8531" without distinguishing the S part, and the
        // two differ only in the revision nibble. Matching at model level accepts
        // either rather than failing T1 on a variant nobody wrote down.
        assert!(is_yt853x(YT8531));
        assert!(is_yt853x(YT8531S));
    }

    #[test]
    fn a_different_motorcomm_part_does_not_match() {
        // Same vendor OUI, different model — YT8821 is a 2.5G part. Matching the
        // vendor alone would accept it and report a link this driver cannot drive.
        assert!(!is_yt853x(0x4F51_EA19));
        assert!(!is_yt853x(0x4F51_E928), "YT8522 is a different model too");
    }

    #[test]
    fn the_two_poison_words_are_not_a_phy() {
        // A floating MDIO bus reads all-ones; a PHY held in reset reads zero. Both
        // must be refused — this is T1's whole value as an oracle, that a real ID
        // cannot arise from noise.
        assert!(!is_yt853x(0xFFFF_FFFF));
        assert!(!is_yt853x(0x0000_0000));
    }

    #[test]
    fn link_status_needs_two_reads_because_it_latches_low() {
        // BMSR's link bit is latch-low: it reports a link that has gone down *since
        // the last read*, so a single read of a healthy link that blipped earlier
        // returns 0. The second read is the current state. Getting this wrong
        // produces "link is down" on a link that is up — and then hours spent on
        // cables and RGMII delays.
        let seq = |a: u16, b: u16| {
            let mut reads = [a, b].into_iter();
            link_is_up(move || reads.next().unwrap_or(0))
        };
        assert!(!seq(0, 0), "genuinely down");
        assert!(seq(BMSR_LINK_UP, BMSR_LINK_UP), "genuinely up");
        assert!(seq(0, BMSR_LINK_UP), "blipped, but up now");
        assert!(!seq(BMSR_LINK_UP, 0), "was up, now down — trust the second");
    }

    #[test]
    fn checking_the_link_always_costs_exactly_two_reads() {
        // The count is the property. One read is the latch-low bug; three would mean
        // a caller could observe a different value than the one decided on.
        let mut count = 0;
        link_is_up(|| {
            count += 1;
            BMSR_LINK_UP
        });
        assert_eq!(count, 2);
    }

    #[test]
    fn advertising_100_full_offers_only_what_we_can_drive() {
        let anar = advertise_100_full();
        assert_ne!(anar & ANAR_100_FULL, 0, "the mode we want");
        assert_eq!(anar & ANAR_SELECTOR_MASK, ANAR_SELECTOR_802_3, "802.3 selector");
        // Deliberately not offering gigabit: forcing 100 removes the 125 MHz GTXCLK
        // path and the gigabit RGMII timing margin — three fewer bring-up variables
        // for a link that only ever carries telemetry. See the design note.
        assert_eq!(anar & ANAR_100_HALF, 0, "half duplex is not wanted either");
    }

    #[test]
    fn restarting_autoneg_both_enables_and_restarts_it() {
        // Setting RESTART without ENABLE is a no-op on most PHYs, and it is a silent
        // one — autoneg simply never completes and link never comes up.
        let bmcr = restart_autoneg();
        assert_ne!(bmcr & BMCR_ANEG_ENABLE, 0);
        assert_ne!(bmcr & BMCR_ANEG_RESTART, 0);
        assert_eq!(bmcr & BMCR_RESET, 0, "a restart is not a reset");
    }

    #[test]
    fn autoneg_is_only_done_when_both_it_and_the_link_report_ready() {
        assert!(negotiation_complete(BMSR_ANEG_COMPLETE | BMSR_LINK_UP));
        assert!(!negotiation_complete(BMSR_LINK_UP), "link but no negotiation");
        assert!(!negotiation_complete(BMSR_ANEG_COMPLETE), "negotiated but no link");
    }

    #[test]
    fn the_bit_constants_sit_where_the_standard_puts_them() {
        // Pinned as literals, independently of the shifts that build them: an
        // assertion of the form `x & MASK == 0` passes vacuously when MASK is zero,
        // so without this the "a restart is not a reset" and "half duplex is not
        // wanted" checks below would hold against a broken constant.
        assert_eq!(BMCR_RESET, 0x8000);
        assert_eq!(BMCR_ANEG_ENABLE, 0x1000);
        assert_eq!(BMCR_ANEG_RESTART, 0x0200);
        assert_eq!(BMSR_LINK_UP, 0x0004);
        assert_eq!(BMSR_ANEG_COMPLETE, 0x0020);
        assert_eq!(ANAR_100_FULL, 0x0100);
        assert_eq!(ANAR_100_HALF, 0x0080);
    }

    #[test]
    fn the_register_numbers_are_the_ones_the_standard_assigns() {
        // Pinned as literals: these are clause-22's fixed assignments, and a
        // derivation would only agree with itself.
        assert_eq!(BMCR, 0);
        assert_eq!(BMSR, 1);
        assert_eq!(PHY_ID1, 2);
        assert_eq!(PHY_ID2, 3);
        assert_eq!(ANAR, 4);
    }
}
