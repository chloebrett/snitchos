//! Render a captured `RAMFBCfg` framebuffer region as a PPM (P6) image —
//! the visual proof-of-correctness `--dump-framebuffer` uses in place of a
//! browser canvas (which snemu has no groundwork for yet; see
//! `plans/snemu-ramfb-model.md`). PPM is about the simplest image format
//! that exists: a text header, then raw RGB bytes, row-major — any image
//! viewer opens it, and it's trivial to write with zero dependencies.

/// Render `pixels` (a byte region covering exactly `stride * height` bytes,
/// row-major, `stride` bytes per row) as a binary PPM (P6) image of
/// `width` × `height`. Pure — no I/O, no `Memory`/`Machine` dependency, so
/// it's fully host-testable in isolation; the caller (`Machine::
/// dump_framebuffer`) is the thin, non-pure wrapper that extracts `pixels`
/// from guest RAM.
///
/// Each pixel is 4 bytes, little-endian XRGB8888 — `[B, G, R, pad]`,
/// matching `kernel_core::framebuffer::color_to_bytes`'s byte order exactly
/// (the real display byte order, independent of `fw_cfg`'s big-endian wire
/// format for the *config*). PPM wants 3-byte RGB triples, so the pad byte
/// is dropped and B/R are swapped into R/G/B order.
///
/// `stride` may exceed `width * 4` (row padding) — only the first
/// `width * 4` bytes of each `stride`-byte row are pixels; the rest is
/// skipped, same convention as `kernel_core::framebuffer::Framebuffer`.
/// Out-of-range reads (a `pixels` slice shorter than `stride * height`,
/// e.g. a corrupt/truncated capture) degrade to black rather than
/// panicking — a possibly-wrong image is more useful for debugging than a
/// crash.
pub fn render_ppm(pixels: &[u8], width: u32, height: u32, stride: u32) -> Vec<u8> {
    let mut out = format!("P6\n{width} {height}\n255\n").into_bytes();
    for y in 0..height {
        let row_start = (y * stride) as usize;
        for x in 0..width {
            let (r, g, b) = decode_pixel(pixels, row_start + (x * 4) as usize);
            out.extend_from_slice(&[r, g, b]);
        }
    }
    out
}

/// Decode one XRGB8888 pixel's `(r, g, b)` channels from `pixels` at byte
/// offset `offset` — little-endian `[B, G, R, pad]`, so `pixels[offset]` is
/// blue, `+1` green, `+2` red (the pad byte at `+3` is never read). Missing
/// bytes (an `offset` past `pixels`' end) degrade that channel to `0`
/// individually, not the whole pixel — see `render_ppm`'s degrade-to-black
/// doc for why a partially-out-of-range pixel isn't fully blacked out.
fn decode_pixel(pixels: &[u8], offset: usize) -> (u8, u8, u8) {
    let b = pixels.get(offset).copied().unwrap_or(0);
    let g = pixels.get(offset + 1).copied().unwrap_or(0);
    let r = pixels.get(offset + 2).copied().unwrap_or(0);
    (r, g, b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_names_the_correct_format_and_dimensions() {
        let ppm = render_ppm(&[0u8; 16], 2, 2, 8);
        assert!(ppm.starts_with(b"P6\n2 2\n255\n"));
    }

    #[test]
    fn single_pixel_converts_xrgb8888_to_rgb_dropping_the_pad_byte() {
        // The real clear color, 0x20_20_40 (R=0x20,G=0x20,B=0x40), stored as
        // the kernel writes it: [B, G, R, pad].
        let pixel = [0x40, 0x20, 0x20, 0xff];
        let ppm = render_ppm(&pixel, 1, 1, 4);
        let body = &ppm[ppm.len() - 3..];
        assert_eq!(body, &[0x20, 0x20, 0x40], "R, G, B order, pad byte dropped");
    }

    #[test]
    fn stride_wider_than_width_times_four_skips_row_padding() {
        // width=1 (4 pixel bytes) but stride=8 (4 bytes of padding per row).
        // Row 0's pixel is [0x01,0x02,0x03,0xff]; bytes 4..8 are padding
        // that must NOT leak into row 1's output.
        let mut pixels = vec![0u8; 16];
        pixels[0..4].copy_from_slice(&[0x01, 0x02, 0x03, 0xff]); // row 0 pixel
        pixels[8..12].copy_from_slice(&[0x04, 0x05, 0x06, 0xff]); // row 1 pixel
        let ppm = render_ppm(&pixels, 1, 2, 8);
        let body = &ppm[ppm.len() - 6..];
        assert_eq!(body, &[0x03, 0x02, 0x01, 0x06, 0x05, 0x04]);
    }

    #[test]
    fn a_fully_out_of_range_pixel_degrades_to_black_not_panic() {
        let ppm = render_ppm(&[], 1, 1, 4); // no bytes at all
        let body = &ppm[ppm.len() - 3..];
        assert_eq!(body, &[0, 0, 0]);
    }

    #[test]
    fn a_partially_out_of_range_pixel_degrades_only_the_missing_channels() {
        // Only 2 of the pixel's 4 bytes exist: B=0xAB, G=0xCD present; the
        // R byte (index 2) and pad byte (index 3, unused) are missing.
        // Per-channel degradation, not "any missing byte blacks out the
        // whole pixel" — more informative for a corrupt/truncated capture.
        let ppm = render_ppm(&[0xAB, 0xCD], 1, 1, 4);
        let body = &ppm[ppm.len() - 3..];
        assert_eq!(body, &[0, 0xCD, 0xAB], "R missing (0), G and B present");
    }

    #[test]
    fn multi_row_multi_column_produces_the_right_pixel_count() {
        let ppm = render_ppm(&[0u8; 4 * 3 * 2], 3, 2, 12);
        let header_len = b"P6\n3 2\n255\n".len();
        assert_eq!(ppm.len() - header_len, 3 * 2 * 3, "width*height RGB triples");
    }
}
