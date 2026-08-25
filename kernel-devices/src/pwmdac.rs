//! JH7110 PWMDAC control-register *layout* logic — the part that decides what
//! `u32` to write to `CTRL`, no MMIO. Drives the `VisionFive` 2's 3.5mm analog
//! audio-out jack. The kernel driver (`kernel/src/device/pwmdac.rs`) does the
//! volatile writes at these offsets; what's here is pure and host-tested.
//!
//! The block is two registers: `WDATA` (sample port, offset 0x00) and `CTRL`
//! (offset 0x04). Field positions and enum encodings are transcribed verbatim
//! from mainline Linux `sound/soc/starfive/jh7110_pwmdac.c`. See
//! `docs/vf2-audio-design.md` for the wider design and `plans/legacy/vf2-audio-tier0.md`
//! for the increment plan.
//!
//! `CTRL` layout: `ENABLE[0]`, `SHIFT[1]` (8- vs 10-bit resolution),
//! `DUTY_CYCLE[3:2]`, `CNT_N[12:4]` (sample-count / rate divider),
//! `DATA_CHANGE[13]`, `DATA_MODE[14]`, `DATA_SHIFT[17:15]`.

/// Byte offset of the sample-data write port (`JH7110_PWMDAC_WDATA`).
pub const WDATA_OFFSET: usize = 0x00;
/// Byte offset of the control register (`JH7110_PWMDAC_CTRL`).
pub const CTRL_OFFSET: usize = 0x04;

/// Output resolution — `CTRL.SHIFT` (bit 1). Mainline: `PWMDAC_SHIFT_8 = 0`,
/// `PWMDAC_SHIFT_10 = 1`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resolution {
    Bits8 = 0,
    Bits10 = 1,
}

/// PWM duty-cycle mode — `CTRL.DUTY_CYCLE` (bits 3:2). Mainline:
/// `PWMDAC_CYCLE_LEFT = 0`, `PWMDAC_CYCLE_RIGHT = 1`, `PWMDAC_CYCLE_CENTER = 2`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DutyCycle {
    Left = 0,
    Right = 1,
    Center = 2,
}

/// Sample data interpretation — `CTRL.DATA_MODE` (bit 14). Mainline:
/// `UNSIGNED_DATA = 0`, `INVERTER_DATA_MSB = 1`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DataMode {
    Unsigned = 0,
    InverterMsb = 1,
}

/// Sample-count / rate divider — the `CTRL.CNT_N` field (bits 12:4), a **9-bit**
/// field valid for `1..=511`.
///
/// Note the discrepancy this newtype guards: mainline's `enum` lists
/// `PWMDAC_SAMPLE_CNT_512 = 512`, but `GENMASK(12, 4)` is only 9 bits wide and
/// cannot hold 512. `new` rejects it; whether the hardware field is actually
/// wider is an open datasheet question (see `plans/legacy/vf2-audio-tier0.md`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CntN(u16);

impl CntN {
    /// The `CNT_N` field is `GENMASK(12, 4)` — 9 bits, so `1..=511`. Zero is not a
    /// valid sample count.
    #[must_use]
    pub fn new(n: u16) -> Option<Self> {
        (1..=511).contains(&n).then_some(Self(n))
    }

    #[must_use]
    pub fn get(self) -> u16 {
        self.0
    }
}

/// Data left-shift amount — the `CTRL.DATA_SHIFT` field (bits 17:15), a **3-bit**
/// field valid for `0..=7` (mainline `PWMDAC_DATA_LEFT_SHIFT_BIT_0..7`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DataShift(u8);

impl DataShift {
    /// The `DATA_SHIFT` field is `GENMASK(17, 15)` — 3 bits, so `0..=7`.
    #[must_use]
    pub fn new(n: u8) -> Option<Self> {
        (n <= 7).then_some(Self(n))
    }

    #[must_use]
    pub fn get(self) -> u8 {
        self.0
    }
}

/// A fully-specified `CTRL` register value, encodable to the `u32` the kernel
/// driver writes at `CTRL_OFFSET`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ctrl {
    pub enable: bool,
    pub resolution: Resolution,
    pub duty_cycle: DutyCycle,
    pub cnt_n: CntN,
    pub data_change: bool,
    pub data_mode: DataMode,
    pub data_shift: DataShift,
}

impl Ctrl {
    /// Encode to the `u32` written to the `CTRL` register.
    ///
    /// The fields occupy disjoint bit ranges — enforced by the `CntN` (`≤ 511`,
    /// bits 12:4) and `DataShift` (`≤ 7`, bits 17:15) bounds and the small enum
    /// discriminants — so no two `|` operands ever share a set bit. That makes
    /// `|` and `^` equivalent here: mutation testing reports the `| → ^` mutants
    /// as survivors, and they are genuine equivalent mutants, not a test gap.
    #[must_use]
    pub fn to_bits(&self) -> u32 {
        u32::from(self.enable)
            | ((self.resolution as u32) << 1)
            | ((self.duty_cycle as u32) << 2)
            | (u32::from(self.cnt_n.get()) << 4)
            | (u32::from(self.data_change) << 13)
            | ((self.data_mode as u32) << 14)
            | (u32::from(self.data_shift.get()) << 15)
    }
}

/// A programmed sample rate: the `pwmdac_core` clock to request via the SYSCRG and
/// the `CNT_N` oversampling divider that together realise it.
///
/// The relationship is `core_clk = sample_rate × cnt_n × 256`, where 256 = 2⁸ is
/// the 8-bit PWM period (`Resolution::Bits8`). `cnt_n` is chosen per rate so the
/// core clock lands on a value the audio PLL can actually synthesise — which is why
/// it varies (3 at 8 kHz, 1 at 44.1/48 kHz). Transcribed from mainline
/// `jh7110_pwmdac_hw_params`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RatePlan {
    pub core_clk_hz: u32,
    pub cnt_n: CntN,
}

/// Plan the `pwmdac_core` clock + `CNT_N` divider for a PCM sample rate, or `None`
/// if unsupported. Mainline supports 8000/11025/16000/22050/32000/44100/48000 Hz
/// (`SNDRV_PCM_RATE_8000_48000`); anything else the driver rejects with `-EINVAL`.
#[must_use]
pub fn plan_rate(sample_rate_hz: u32) -> Option<RatePlan> {
    let (core_clk_hz, cnt_n) = match sample_rate_hz {
        8000 => (6_144_000, 3),
        11025 => (5_644_800, 2),
        16000 => (12_288_000, 3),
        22050 => (5_644_800, 1),
        32000 => (8_192_000, 1),
        44100 => (11_289_600, 1),
        48000 => (12_288_000, 1),
        _ => return None,
    };
    Some(RatePlan { core_clk_hz, cnt_n: CntN::new(cnt_n)? })
}

/// Timer ticks between consecutive `WDATA` writes to emit at `sample_rate_hz`,
/// given a timebase of `timer_hz` (the VF2's is 4 MHz, read from the DTB). `None`
/// for a zero sample rate.
///
/// Rounded to nearest, so per-sample pacing carries up to half a tick of error —
/// a small fixed pitch offset, fine for a fixed-frequency beep. A fractional-phase
/// accumulator (drift-free) is a Tier 1 / streaming concern, not this.
#[must_use]
pub fn sample_interval_ticks(sample_rate_hz: u32, timer_hz: u64) -> Option<u64> {
    let fs = u64::from(sample_rate_hz);
    (fs != 0).then(|| (timer_hz + fs / 2) / fs)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal valid config: disabled, everything at its zero-valued variant,
    /// `cnt_n = 1` (the field's minimum). Its only set bits are `CNT_N`.
    fn base() -> Ctrl {
        Ctrl {
            enable: false,
            resolution: Resolution::Bits8,
            duty_cycle: DutyCycle::Left,
            cnt_n: CntN::new(1).expect("1 is in range"),
            data_change: false,
            data_mode: DataMode::Unsigned,
            data_shift: DataShift::new(0).expect("0 is in range"),
        }
    }

    #[test]
    fn register_offsets_match_the_datasheet() {
        assert_eq!(WDATA_OFFSET, 0x00, "WDATA is the sample port");
        assert_eq!(CTRL_OFFSET, 0x04, "CTRL follows it");
    }

    #[test]
    fn baseline_sets_only_the_cnt_n_field() {
        // cnt_n = 1, shifted into bits 12:4 → 1 << 4.
        assert_eq!(base().to_bits(), 0x10);
    }

    #[test]
    fn enable_sets_bit_0() {
        assert_eq!(Ctrl { enable: true, ..base() }.to_bits(), 0x10 | 0x1);
    }

    #[test]
    fn ten_bit_resolution_sets_shift_bit_1() {
        assert_eq!(
            Ctrl { resolution: Resolution::Bits10, ..base() }.to_bits(),
            0x10 | (1 << 1)
        );
    }

    #[test]
    fn duty_cycle_center_lands_in_bits_3_2() {
        // CENTER = 2, field at shift 2 → 2 << 2 = 0b1000.
        assert_eq!(
            Ctrl { duty_cycle: DutyCycle::Center, ..base() }.to_bits(),
            0x10 | (2 << 2)
        );
    }

    #[test]
    fn cnt_n_occupies_bits_12_through_4() {
        // 511 (the field max) fills all nine bits, nothing spills above bit 12.
        let ctrl = Ctrl { cnt_n: CntN::new(511).expect("511 is the max"), ..base() };
        assert_eq!(ctrl.to_bits(), 511 << 4);
        assert_eq!(ctrl.to_bits() & !0x1FF0, 0, "nothing set outside bits 12:4");
    }

    #[test]
    fn data_change_is_bit_13() {
        assert_eq!(Ctrl { data_change: true, ..base() }.to_bits(), 0x10 | (1 << 13));
    }

    #[test]
    fn data_mode_inverter_is_bit_14() {
        assert_eq!(
            Ctrl { data_mode: DataMode::InverterMsb, ..base() }.to_bits(),
            0x10 | (1 << 14)
        );
    }

    #[test]
    fn data_shift_lands_in_bits_17_through_15() {
        let ctrl = Ctrl { data_shift: DataShift::new(7).expect("7 is the max"), ..base() };
        assert_eq!(ctrl.to_bits(), 0x10 | (7 << 15));
    }

    #[test]
    fn mainline_default_init_word() {
        // The driver's power-on config: enabled, 8-bit, centre duty, inverter-MSB,
        // no data-shift, cnt_n = 1. Documents the composite we expect on the wire.
        let ctrl = Ctrl {
            enable: true,
            resolution: Resolution::Bits8,
            duty_cycle: DutyCycle::Center,
            cnt_n: CntN::new(1).expect("1 is in range"),
            data_change: false,
            data_mode: DataMode::InverterMsb,
            data_shift: DataShift::new(0).expect("0 is in range"),
        };
        // 0x1 (enable) | 0x8 (centre) | 0x10 (cnt_n=1) | 0x4000 (inverter) = 0x4019.
        assert_eq!(ctrl.to_bits(), 0x4019);
    }

    #[test]
    fn cnt_n_rejects_zero_and_values_past_the_9_bit_field() {
        assert!(CntN::new(0).is_none(), "0 is not a valid sample count");
        // 512 is the mainline enum's max but overflows GENMASK(12,4).
        assert!(CntN::new(512).is_none(), "512 does not fit a 9-bit field");
        assert!(CntN::new(1).is_some());
        assert!(CntN::new(511).is_some());
    }

    #[test]
    fn data_shift_rejects_values_past_the_3_bit_field() {
        assert!(DataShift::new(8).is_none(), "8 does not fit a 3-bit field");
        assert!(DataShift::new(7).is_some());
        assert_eq!(DataShift::new(5).expect("in range").get(), 5);
    }

    /// The seven `(sample_rate, core_clk, cnt_n)` rows the mainline driver's
    /// `hw_params` switch encodes.
    const RATE_TABLE: [(u32, u32, u16); 7] = [
        (8000, 6_144_000, 3),
        (11025, 5_644_800, 2),
        (16000, 12_288_000, 3),
        (22050, 5_644_800, 1),
        (32000, 8_192_000, 1),
        (44100, 11_289_600, 1),
        (48000, 12_288_000, 1),
    ];

    #[test]
    fn supported_rates_map_to_the_mainline_clock_and_divider() {
        for (fs, core, n) in RATE_TABLE {
            let expected = RatePlan { core_clk_hz: core, cnt_n: CntN::new(n).expect("in range") };
            assert_eq!(plan_rate(fs), Some(expected), "rate {fs} Hz");
        }
    }

    #[test]
    fn unsupported_rates_are_rejected() {
        for fs in [0, 7999, 44099, 88200, 96000, 192_000] {
            assert!(plan_rate(fs).is_none(), "rate {fs} Hz is unsupported");
        }
    }

    #[test]
    fn core_clock_is_sample_rate_times_cnt_n_times_the_8bit_period() {
        // Documents the formula the table encodes: core = fs × cnt_n × 2⁸.
        for (fs, _, _) in RATE_TABLE {
            let plan = plan_rate(fs).expect("supported");
            assert_eq!(plan.core_clk_hz, fs * u32::from(plan.cnt_n.get()) * 256, "rate {fs} Hz");
        }
    }

    #[test]
    fn interval_for_8k_at_the_4mhz_vf2_timebase_is_500_ticks() {
        // 4_000_000 / 8000 = 500, exactly — the beep's target rate on the board.
        assert_eq!(sample_interval_ticks(8000, 4_000_000), Some(500));
    }

    #[test]
    fn interval_rounds_to_the_nearest_tick() {
        // 4_000_000 / 44100 = 90.70… → 91 (nearest), not 90 (truncated).
        assert_eq!(sample_interval_ticks(44100, 4_000_000), Some(91));
        // 4_000_000 / 48000 = 83.33… → 83 (nearest).
        assert_eq!(sample_interval_ticks(48000, 4_000_000), Some(83));
    }

    #[test]
    fn zero_sample_rate_is_rejected() {
        assert!(sample_interval_ticks(0, 4_000_000).is_none());
    }
}
