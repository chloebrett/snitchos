//! The `glitch` server's pure synthesis policy: turn a client's `Play` request
//! into the sample stream the server feeds to `AudioWrite`. Host-testable — no
//! IPC, no syscalls, no runtime dep — so the count / amplitude / rejection logic
//! is unit-tested here, while `user/glitch` holds only the riscv-only `serve`
//! glue. Split for the same reason `fs-core` is split out of `user/fs`. See
//! `plans/glitch.md` (Increment 5c).

#![no_std]
#![forbid(unsafe_code)]

use glitch_proto::Play;
use synth::{Gain, Tone};

/// DAC sample rate the server synthesizes at. **Must match the kernel's PWMDAC
/// configured rate** (`BEEP_RATE_HZ` in `kernel/src/device/pwmdac.rs`): the kernel
/// paces `WDATA` at this rate, so samples generated at any other rate would play
/// at the wrong pitch. v1 fixes it; rate negotiation is a v2 non-goal.
pub const FS_HZ: u32 = 8000;

/// The server's fixed output amplitude — deliberately low (a square wave is harsh
/// and this drives headphones), matching the in-kernel beep's `BEEP_PEAK`. Volume
/// is the *server's* policy in v1: clients name the note, not the amplitude.
pub const PEAK: i16 = 4000;

/// Turn a `Play` request into the samples to emit — a square wave at the requested
/// frequency and the server's fixed amplitude, repeated to fill `duration_ms` at
/// [`FS_HZ`]. `None` if the frequency is unsynthesizable (0 or ≥ Nyquist), which
/// the server answers with `Reply::Refused`.
#[must_use]
pub fn plan_play(req: Play) -> Option<impl Iterator<Item = i16>> {
    let tone = Tone::square(req.freq_hz, FS_HZ, Gain::UNITY, PEAK)?;
    let count = (u64::from(req.duration_ms) * u64::from(FS_HZ) / 1000) as u32;
    Some((0..count).map(move |i| tone.sample_at(i)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plans_one_second_as_fs_samples() {
        let n = plan_play(Play { freq_hz: 440, duration_ms: 1000 }).expect("valid").count();
        assert_eq!(n, 8000, "1 s at 8 kHz is 8000 samples");
    }

    #[test]
    fn duration_scales_the_sample_count() {
        let n = plan_play(Play { freq_hz: 440, duration_ms: 250 }).expect("valid").count();
        assert_eq!(n, 2000, "250 ms at 8 kHz is 2000 samples");
    }

    #[test]
    fn sample_count_is_fs_scaled_by_duration() {
        // Asserted directly (not via `.count()`) so a `/1000 → *1000` mutation dies
        // on this fast check instead of hanging a billion-sample iterator.
        assert_eq!(sample_count(1000), 8000, "1 s at 8 kHz");
        assert_eq!(sample_count(250), 2000, "250 ms at 8 kHz");
        assert_eq!(sample_count(0), 0, "a zero-duration play is empty");
    }

    #[test]
    fn synthesizes_at_the_server_amplitude() {
        let first = plan_play(Play { freq_hz: 440, duration_ms: 1000 }).expect("valid").next();
        assert_eq!(first, Some(PEAK), "the square opens high at the server's fixed peak");
    }

    #[test]
    fn refuses_a_zero_frequency() {
        assert!(plan_play(Play { freq_hz: 0, duration_ms: 1000 }).is_none());
    }

    #[test]
    fn refuses_a_supra_nyquist_frequency() {
        // A square needs ≥ 2 samples/period; freq == fs gives period 1.
        assert!(plan_play(Play { freq_hz: FS_HZ, duration_ms: 1000 }).is_none());
    }
}
