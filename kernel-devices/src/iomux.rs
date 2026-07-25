//! JH7110 SYS_IOMUX (pin-mux) register-layout logic — no MMIO. Routes a
//! peripheral's output signal onto a physical GPIO pad, the step that gets the
//! PWMDAC's PWM to the 3.5mm jack pins (GPIO 33 = LEFT, GPIO 34 = RIGHT on the
//! VisionFive 2). Without it the DAC runs but its output reaches no pad → silence.
//!
//! Model (from mainline `pinctrl-starfive-jh7110-sys.c` + `jh7110-common.dtsi`):
//! each pad's output signal lives in an 8-bit field in the `DOUT` bank and its
//! output-enable in the `DOEN` bank, four pads per 32-bit word. A pad `g` sits at
//! word `base + 4*(g/4)`, bit-field `8*(g%4)`. Output-enable is **active-low**
//! (`GPOEN_ENABLE = 0`). Transcribed from the board DTS's `pwmdac_pins` group.

/// SYS_IOMUX MMIO base on JH7110. Shares SYSCRG's 2 MiB megapage (`0x1300_0000`),
/// so `kmain`'s SYSCRG `insert` already maps it.
pub const BASE: usize = 0x1304_0000;

const DOEN_BASE: usize = 0x000;
const DOUT_BASE: usize = 0x040;

/// Output-enable value that turns the pad's driver **on** (`GPOEN_ENABLE`, active-low).
const OEN_ENABLE: u32 = 0;

/// VF2 PWMDAC pads + their output signals (`GPOUT_SYS_PWMDAC_LEFT/RIGHT`).
pub const PWMDAC_LEFT_GPIO: u32 = 33;
pub const PWMDAC_LEFT_SIGNAL: u32 = 28;
pub const PWMDAC_RIGHT_GPIO: u32 = 34;
pub const PWMDAC_RIGHT_SIGNAL: u32 = 29;

/// A read-modify-write to one 8-bit pin field: `reg[offset] = (reg & !mask) | value`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FieldWrite {
    pub offset: usize,
    pub mask: u32,
    pub value: u32,
}

/// Route pad `gpio`'s output to `signal` and enable its output driver — the two
/// writes (`DOUT` ← signal, `DOEN` ← enabled) that mux a peripheral output onto a
/// pin. Input routing (`GPI`) is left alone: the PWMDAC pads are output-only.
#[must_use]
pub fn route_output(gpio: u32, signal: u32) -> [FieldWrite; 2] {
    [
        field(DOUT_BASE, gpio, signal),
        field(DOEN_BASE, gpio, OEN_ENABLE),
    ]
}

/// One 8-bit pad field: `4*(gpio/4)` picks the word, `8*(gpio%4)` the byte lane.
fn field(bank: usize, gpio: u32, value: u32) -> FieldWrite {
    let offset = bank + 4 * (gpio / 4) as usize;
    let shift = 8 * (gpio % 4);
    FieldWrite { offset, mask: 0xFF << shift, value: (value & 0xFF) << shift }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_and_signals_match_the_board_dts() {
        assert_eq!(BASE, 0x1304_0000);
        assert_eq!((PWMDAC_LEFT_GPIO, PWMDAC_LEFT_SIGNAL), (33, 28));
        assert_eq!((PWMDAC_RIGHT_GPIO, PWMDAC_RIGHT_SIGNAL), (34, 29));
    }

    #[test]
    fn left_pad_routes_dout_signal_and_enables_output() {
        // GPIO 33: word 33/4 = 8 → offset 4*8 = 0x20; field 8*(33%4) = bits[15:8].
        let [dout, doen] = route_output(PWMDAC_LEFT_GPIO, PWMDAC_LEFT_SIGNAL);
        assert_eq!(dout, FieldWrite { offset: DOUT_BASE + 0x20, mask: 0xFF << 8, value: 28 << 8 });
        assert_eq!(doen, FieldWrite { offset: DOEN_BASE + 0x20, mask: 0xFF << 8, value: 0 });
    }

    #[test]
    fn right_pad_lands_in_the_same_word_next_field() {
        // GPIO 34: same word (34/4 = 8), field 8*(34%4) = bits[23:16].
        let [dout, doen] = route_output(PWMDAC_RIGHT_GPIO, PWMDAC_RIGHT_SIGNAL);
        assert_eq!(dout, FieldWrite { offset: DOUT_BASE + 0x20, mask: 0xFF << 16, value: 29 << 16 });
        assert_eq!(doen, FieldWrite { offset: DOEN_BASE + 0x20, mask: 0xFF << 16, value: 0 });
    }

    #[test]
    fn signal_is_masked_to_eight_bits_and_never_bleeds_neighbours() {
        let [dout, _] = route_output(PWMDAC_LEFT_GPIO, PWMDAC_LEFT_SIGNAL);
        assert_eq!(dout.value & !dout.mask, 0, "value stays inside its field");
    }
}
