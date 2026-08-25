//! Render a captured PWMDAC sample stream as a WAV file — the by-ear
//! proof-of-correctness `--audio-out` uses, the audio counterpart to
//! `framebuffer::render_ppm`. WAV is about the simplest audio container that
//! exists: a fixed 44-byte canonical-PCM header, then raw little-endian samples —
//! every audio player opens it, and it's trivial to write with zero dependencies.
//!
//! Pure — no I/O, no `Machine`/`Bus` dependency — so it's fully host-testable in
//! isolation; the caller (the `--audio-out` wrapper in `main`) is the thin,
//! non-pure part that pulls the captured samples out of the PWMDAC device and
//! writes the bytes to disk. See `plans/legacy/vf2-audio-tier0.md` Increment 9a.

/// Encode `samples` (signed 16-bit PCM) as a canonical mono WAV byte stream at
/// `sample_rate_hz`. The header is the standard 44-byte RIFF/WAVE/fmt/data layout;
/// samples follow, each little-endian. An empty `samples` yields a valid
/// header-only (44-byte) file.
pub fn encode_wav_mono_16(sample_rate_hz: u32, samples: &[i16]) -> Vec<u8> {
    const BITS_PER_SAMPLE: u16 = 16;
    const NUM_CHANNELS: u16 = 1;
    let block_align = NUM_CHANNELS * (BITS_PER_SAMPLE / 8);
    let byte_rate = sample_rate_hz * u32::from(block_align);
    let data_len = (samples.len() * 2) as u32;

    let mut out = Vec::with_capacity(44 + samples.len() * 2);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // PCM fmt chunk size
    out.extend_from_slice(&1u16.to_le_bytes()); // audio format: PCM
    out.extend_from_slice(&NUM_CHANNELS.to_le_bytes());
    out.extend_from_slice(&sample_rate_hz.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&BITS_PER_SAMPLE.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for &sample in samples {
        out.extend_from_slice(&sample.to_le_bytes());
    }
    out
}

/// Recover the effective sample rate (Hz) from the guest-clock `timestamps` of
/// consecutive `WDATA` writes, given the guest `timer_hz`. `N` writes span `N-1`
/// intervals over `last - first` ticks, so the rate is
/// `(N-1) * timer_hz / (last - first)`, rounded to nearest.
///
/// `None` if there are fewer than two writes, or if the span is zero/non-increasing
/// — no rate is defined. Because it measures the *actual* inter-write timing, it
/// reflects any drift from Increment 5's integer pacing rather than the nominal
/// rate — which is the point: snemu renders what the guest really produced.
pub fn reconstruct_sample_rate(timestamps: &[u64], timer_hz: u64) -> Option<u32> {
    let first = *timestamps.first()?;
    let last = *timestamps.last()?;
    let intervals = (timestamps.len() as u64).checked_sub(1)?;
    let span = last.checked_sub(first)?;
    if intervals == 0 || span == 0 {
        return None;
    }
    Some(((intervals * timer_hz + span / 2) / span) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u16_at(wav: &[u8], off: usize) -> u16 {
        u16::from_le_bytes(wav[off..off + 2].try_into().expect("2 bytes"))
    }

    fn u32_at(wav: &[u8], off: usize) -> u32 {
        u32::from_le_bytes(wav[off..off + 4].try_into().expect("4 bytes"))
    }

    #[test]
    fn header_only_for_empty_samples_is_44_bytes() {
        assert_eq!(encode_wav_mono_16(8000, &[]).len(), 44);
    }

    #[test]
    fn total_length_is_header_plus_two_bytes_per_sample() {
        let wav = encode_wav_mono_16(8000, &[0, 0, 0]);
        assert_eq!(wav.len(), 44 + 3 * 2);
    }

    #[test]
    fn riff_wave_fmt_and_data_tags_are_at_the_canonical_offsets() {
        let wav = encode_wav_mono_16(8000, &[0]);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(&wav[12..16], b"fmt ");
        assert_eq!(&wav[36..40], b"data");
    }

    #[test]
    fn fmt_chunk_declares_mono_16bit_pcm() {
        let wav = encode_wav_mono_16(8000, &[]);
        assert_eq!(u32_at(&wav, 16), 16, "fmt chunk size = 16 for PCM");
        assert_eq!(u16_at(&wav, 20), 1, "audio format 1 = PCM");
        assert_eq!(u16_at(&wav, 22), 1, "1 channel = mono");
        assert_eq!(u16_at(&wav, 32), 2, "block align = channels * bytes/sample");
        assert_eq!(u16_at(&wav, 34), 16, "16 bits per sample");
    }

    #[test]
    fn encodes_sample_rate_and_derived_byte_rate() {
        let wav = encode_wav_mono_16(44_100, &[]);
        assert_eq!(u32_at(&wav, 24), 44_100, "sample rate");
        assert_eq!(u32_at(&wav, 28), 44_100 * 2, "byte rate = rate * blockalign");
    }

    #[test]
    fn riff_chunk_size_is_36_plus_data_length() {
        let wav = encode_wav_mono_16(8000, &[1, 2, 3, 4]);
        assert_eq!(u32_at(&wav, 4), 36 + 4 * 2);
    }

    #[test]
    fn data_chunk_size_and_samples_are_little_endian() {
        // -1 → 0xFFFF (FF FF); 256 → 0x0100 (00 01).
        let wav = encode_wav_mono_16(8000, &[-1, 256]);
        assert_eq!(u32_at(&wav, 40), 2 * 2, "data size = 2 samples * 2 bytes");
        assert_eq!(&wav[44..48], &[0xFF, 0xFF, 0x00, 0x01]);
    }

    #[test]
    fn evenly_paced_writes_recover_the_source_rate() {
        // 500-tick spacing at the VF2's 4 MHz timebase → 8 kHz (Increment 5).
        let ts = [0, 500, 1000, 1500];
        assert_eq!(reconstruct_sample_rate(&ts, 4_000_000), Some(8000));
    }

    #[test]
    fn two_writes_are_enough() {
        // One 250-tick interval at 4 MHz → 16 kHz.
        assert_eq!(reconstruct_sample_rate(&[0, 250], 4_000_000), Some(16_000));
    }

    #[test]
    fn fewer_than_two_writes_has_no_rate() {
        assert!(reconstruct_sample_rate(&[], 4_000_000).is_none());
        assert!(reconstruct_sample_rate(&[42], 4_000_000).is_none());
    }

    #[test]
    fn zero_or_non_increasing_span_has_no_rate() {
        assert!(reconstruct_sample_rate(&[10, 10], 4_000_000).is_none(), "zero span");
        assert!(reconstruct_sample_rate(&[10, 5], 4_000_000).is_none(), "goes backwards");
    }

    #[test]
    fn rate_rounds_to_nearest() {
        // 1 interval of 3 ticks at 20 Hz → 6.67 → 7.
        assert_eq!(reconstruct_sample_rate(&[0, 3], 20), Some(7));
        // 1 interval of 4 ticks at 10 Hz → exactly 2.5 → rounds up to 3 (the
        // half-way case that pins the `+ span/2` rounding term specifically).
        assert_eq!(reconstruct_sample_rate(&[0, 4], 10), Some(3));
    }
}
