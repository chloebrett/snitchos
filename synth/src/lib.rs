//! Pure signal synthesis — a square-wave `Tone` and a Q0.16 `Gain` — over `core`
//! only. No MMIO, no allocation, no kernel or user-runtime deps: this is the DSP
//! both the kernel beep and the userspace `glitch` server generate samples with.
//!
//! Extracted from `kernel-devices::pwmdac` (glitch Increment 5a) so `user/` can
//! synthesize without depending on a kernel-side crate. The register-layout logic
//! that turns a rate into `CTRL` bits (`Ctrl`/`plan_rate`/`sample_interval_ticks`)
//! stays kernel-side in `kernel-devices`; only the audio math lives here.
//!
//! Deliberately fixed-point, not `f32`: the kernel emits no floating-point at
//! runtime (see `docs/vf2-audio-design.md`), and a shared crate must be usable
//! from that constraint.

#![no_std]
#![forbid(unsafe_code)]

/// Fixed-point linear gain, Q0.16: `0` = silence, `1 << 16` = unity. The volume
/// knob — no hardware gain register exists on the PWMDAC, so amplitude is scaled
/// digitally here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gain(u32);

impl Gain {
    /// Full attenuation — output is mid-scale (0 in signed PCM).
    pub const SILENCE: Gain = Gain(0);
    /// Unity gain — samples pass through at full amplitude.
    pub const UNITY: Gain = Gain(1 << 16);

    /// A gain from a Q0.16 value (`1 << 16` = unity). `None` above unity: this is a
    /// pure attenuator, amplification would clip.
    #[must_use]
    pub fn from_q16(q16: u32) -> Option<Gain> {
        (q16 <= (1 << 16)).then_some(Gain(q16))
    }

    fn apply(self, sample: i16) -> i16 {
        ((i64::from(sample) * i64::from(self.0)) >> 16) as i16
    }
}

/// A generated tone — a fixed number of samples per period at a fixed amplitude.
/// Signed 16-bit PCM, DC-centred at 0; a driver maps this to its sample-port word
/// format at emit time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tone {
    period: u32,
    high: i16,
}

impl Tone {
    /// A square wave at `freq_hz`, sampled at `fs_hz`, scaled by `gain` about a
    /// `peak` amplitude. `None` if `freq_hz` is 0 or at/above Nyquist (fewer than
    /// two samples per period — no meaningful square).
    #[must_use]
    pub fn square(freq_hz: u32, fs_hz: u32, gain: Gain, peak: i16) -> Option<Tone> {
        if freq_hz == 0 {
            return None;
        }
        let period = fs_hz / freq_hz;
        (period >= 2).then_some(Tone { period, high: gain.apply(peak) })
    }

    /// Samples in one period. A driver repeats this for the tone's duration.
    #[must_use]
    pub fn period_samples(self) -> u32 {
        self.period
    }

    /// One period of signed PCM: first half `+high`, second half `-high`.
    pub fn samples(self) -> impl Iterator<Item = i16> {
        let period = self.period;
        let high = self.high;
        (0..period).map(move |i| if i < period / 2 { high } else { -high })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gain_above_unity_is_rejected() {
        assert!(Gain::from_q16((1 << 16) + 1).is_none(), "amplification would clip");
        assert!(Gain::from_q16(1 << 16).is_some(), "unity is the max");
        assert!(Gain::from_q16(0).is_some(), "silence is valid");
    }

    #[test]
    fn square_period_length_is_fs_over_freq() {
        let tone = Tone::square(400, 8000, Gain::UNITY, 1000).expect("valid");
        assert_eq!(tone.period_samples(), 20);
        assert_eq!(tone.samples().count(), 20);
    }

    #[test]
    fn square_is_high_for_the_first_half_then_low() {
        let tone = Tone::square(400, 8000, Gain::UNITY, 1000).expect("valid");
        assert!(tone.samples().take(10).all(|x| x == 1000), "first half high");
        assert!(tone.samples().skip(10).all(|x| x == -1000), "second half low");
    }

    #[test]
    fn unity_gain_reaches_the_peak() {
        let tone = Tone::square(1000, 48000, Gain::UNITY, 20000).expect("valid");
        assert_eq!(tone.samples().next(), Some(20000));
    }

    #[test]
    fn silence_gain_is_all_zero() {
        let tone = Tone::square(1000, 48000, Gain::SILENCE, 20000).expect("valid");
        assert!(tone.samples().all(|x| x == 0));
    }

    #[test]
    fn half_gain_halves_the_amplitude() {
        let half = Gain::from_q16(1 << 15).expect("half is below unity");
        let tone = Tone::square(1000, 48000, half, 20000).expect("valid");
        assert_eq!(tone.samples().next(), Some(10000));
    }

    #[test]
    fn one_period_is_dc_free_for_even_periods() {
        let tone = Tone::square(400, 8000, Gain::UNITY, 1000).expect("period 20 is even");
        let sum: i32 = tone.samples().map(i32::from).sum();
        assert_eq!(sum, 0);
    }

    #[test]
    fn zero_frequency_and_supra_nyquist_are_rejected() {
        assert!(Tone::square(0, 8000, Gain::UNITY, 1000).is_none(), "0 Hz");
        assert!(Tone::square(8000, 8000, Gain::UNITY, 1000).is_none(), "period 1");
        assert!(Tone::square(4000, 8000, Gain::UNITY, 1000).is_some(), "period 2 is the min");
    }
}
