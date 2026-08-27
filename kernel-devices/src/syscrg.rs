//! JH7110 SYSCRG (system clock & reset generator) register-layout logic — no MMIO.
//!
//! The kernel driver (`kernel/src/device/syscrg.rs`) does the volatile read-modify-
//! writes at `BASE + offset`; what's here is the pure model of *which* offsets and
//! bits, transcribed from mainline `clk-starfive-jh71x0.{c,h}`,
//! `reset-starfive-jh71x0.c`, and `reset-starfive-jh7110.c`. Generic enough for any
//! SYSCRG consumer (the display port will want it too); the PWMDAC bring-up
//! sequence is the first client.
//!
//! Clock model: one 32-bit control register per clock at `index * 4`, with the
//! gate at `BIT(31)` and (for divider clocks) the divide in bits `[23:0]`. Reset
//! model: an id maps to assert register `0x2F8 + (id/32)*4` bit `id%32`, and a
//! parallel status register at `0x308 + …`; asserts are active-high (write 1 to
//! hold in reset, 0 to release), and release is confirmed by polling status until
//! the bit reads 1.

/// SYSCRG MMIO base on JH7110 (`clock-controller@13020000`). Its 2 MiB megapage
/// (`0x1300_0000`) is *not* the one `kmain` already maps for UART/virtio — the
/// kernel glue must `insert` it.
pub const BASE: usize = 0x1302_0000;

/// Clock-gate enable bit (`JH71X0_CLK_ENABLE`) — set to 1 to ungate.
pub const CLK_ENABLE: u32 = 1 << 31;
/// Divider field (`JH71X0_CLK_DIV_MASK`, `GENMASK(23, 0)`).
pub const CLK_DIV_MASK: u32 = (1 << 24) - 1;

/// PWMDAC APB gate clock (`JH7110_SYSCLK_PWMDAC_APB`).
pub const PWMDAC_APB_CLK: u32 = 157;
/// PWMDAC core divider clock (`JH7110_SYSCLK_PWMDAC_CORE`), max divide 256.
pub const PWMDAC_CORE_CLK: u32 = 158;
/// PWMDAC APB reset line (`JH7110_SYSRST_PWMDAC_APB`). No core-reset variant exists.
pub const PWMDAC_APB_RESET: u32 = 96;

const RESET_ASSERT_BASE: usize = 0x2F8;
const RESET_STATUS_BASE: usize = 0x308;

/// Byte offset (from [`BASE`]) of the control register for the clock at `index`.
#[must_use]
pub const fn clock_reg_offset(index: u32) -> usize {
    4 * index as usize
}

/// Byte offset (from [`BASE`]) of the assert register holding reset `id`.
#[must_use]
pub const fn reset_assert_offset(id: u32) -> usize {
    RESET_ASSERT_BASE + (id / 32) as usize * 4
}

/// Byte offset (from [`BASE`]) of the status register for reset `id`.
#[must_use]
pub const fn reset_status_offset(id: u32) -> usize {
    RESET_STATUS_BASE + (id / 32) as usize * 4
}

/// The single-bit mask for reset `id` within its assert/status register.
#[must_use]
pub const fn reset_bit(id: u32) -> u32 {
    1 << (id % 32)
}

/// One step of a SYSCRG bring-up sequence, interpreted by the kernel driver.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    /// `reg[offset] = (reg[offset] & !mask) | value` — a read-modify-write.
    Rmw { offset: usize, mask: u32, value: u32 },
    /// Poll `reg[offset]` until `(reg & mask) == mask` — used to confirm a reset
    /// has been released.
    PollUntilSet { offset: usize, mask: u32 },
}

/// The ordered MMIO steps to bring up the PWMDAC: ungate the core (divider clock)
/// and APB clocks, then release the APB reset and wait for it. `core_divider` is
/// `audio_root_rate / core_clk_hz`, supplied by the driver — valid `1..=256` (the
/// clock's `GDIV` maximum). `None` if out of range.
///
/// Parents (`apb0`, critical/always-on; `audio_root`, a boot-set divider; PLL2,
/// from U-Boot) are assumed already running — see `docs/vf2-audio-design.md`.
#[must_use]
pub fn pwmdac_bringup(core_divider: u32) -> Option<[Op; 4]> {
    if !(1..=256).contains(&core_divider) {
        return None;
    }
    let reset = reset_bit(PWMDAC_APB_RESET);
    Some([
        // The gate (bit 31) and divider (bits 23:0) are disjoint, and the masked
        // `core_divider` can't reach bit 31 — so `|` and `^` are equivalent here.
        // Mutation testing flags the `| → ^` mutants as survivors; they are genuine
        // equivalent mutants, as in `pwmdac::Ctrl::to_bits`.
        Op::Rmw {
            offset: clock_reg_offset(PWMDAC_CORE_CLK),
            mask: CLK_DIV_MASK | CLK_ENABLE,
            value: (core_divider & CLK_DIV_MASK) | CLK_ENABLE,
        },
        Op::Rmw {
            offset: clock_reg_offset(PWMDAC_APB_CLK),
            mask: CLK_ENABLE,
            value: CLK_ENABLE,
        },
        Op::Rmw {
            offset: reset_assert_offset(PWMDAC_APB_RESET),
            mask: reset,
            value: 0,
        },
        Op::PollUntilSet {
            offset: reset_status_offset(PWMDAC_APB_RESET),
            mask: reset,
        },
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_control_register_is_index_times_four() {
        assert_eq!(clock_reg_offset(0), 0x00);
        assert_eq!(clock_reg_offset(PWMDAC_APB_CLK), 0x274, "157 * 4");
        assert_eq!(clock_reg_offset(PWMDAC_CORE_CLK), 0x278, "158 * 4");
    }

    #[test]
    fn gate_and_divider_bit_positions_match_the_jh71x0_model() {
        assert_eq!(CLK_ENABLE, 1 << 31, "enable is BIT(31)");
        assert_eq!(CLK_DIV_MASK, 0x00FF_FFFF, "divider is GENMASK(23, 0)");
    }

    #[test]
    fn reset_id_splits_into_word_offset_and_bit() {
        // id 96 → word 3 (96/32), bit 0 (96%32).
        assert_eq!(reset_assert_offset(96), 0x2F8 + 3 * 4, "0x304");
        assert_eq!(reset_status_offset(96), 0x308 + 3 * 4, "0x314");
        assert_eq!(reset_bit(96), 1 << 0);
        // A within-word id exercises the bit arithmetic: id 33 → word 1, bit 1.
        assert_eq!(reset_assert_offset(33), 0x2F8 + 4);
        assert_eq!(reset_bit(33), 1 << 1);
    }

    #[test]
    fn pwmdac_reset_lands_at_0x304_bit_0() {
        assert_eq!(reset_assert_offset(PWMDAC_APB_RESET), 0x304);
        assert_eq!(reset_status_offset(PWMDAC_APB_RESET), 0x314);
        assert_eq!(reset_bit(PWMDAC_APB_RESET), 0x1);
    }

    #[test]
    fn bringup_ungates_core_then_apb_then_releases_reset_and_waits() {
        let ops = pwmdac_bringup(8).expect("divider 8 is valid");
        assert_eq!(
            ops,
            [
                // Core: set the divider (bits 23:0) AND the enable bit.
                Op::Rmw {
                    offset: 0x278,
                    mask: CLK_DIV_MASK | CLK_ENABLE,
                    value: 8 | CLK_ENABLE,
                },
                // APB: gate only.
                Op::Rmw { offset: 0x274, mask: CLK_ENABLE, value: CLK_ENABLE },
                // Release reset: clear bit 0 at the assert register.
                Op::Rmw { offset: 0x304, mask: 0x1, value: 0 },
                // Confirm it: poll the status bit until it reads 1.
                Op::PollUntilSet { offset: 0x314, mask: 0x1 },
            ]
        );
    }

    #[test]
    fn bringup_puts_the_divider_in_the_low_24_bits() {
        let ops = pwmdac_bringup(256).expect("256 is the max");
        let Op::Rmw { value, .. } = ops[0] else { panic!("first op is the core RMW") };
        assert_eq!(value & CLK_DIV_MASK, 256, "divider in bits 23:0");
        assert_eq!(value & CLK_ENABLE, CLK_ENABLE, "enable also set");
    }

    #[test]
    fn bringup_rejects_out_of_range_dividers() {
        assert!(pwmdac_bringup(0).is_none(), "0 is not a valid divide");
        assert!(pwmdac_bringup(257).is_none(), "max GDIV divide is 256");
        assert!(pwmdac_bringup(1).is_some());
        assert!(pwmdac_bringup(256).is_some());
    }
}
