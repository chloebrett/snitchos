//! Pixel operations over a raw XRGB8888 framebuffer. Pure: a view
//! over a caller-provided `&mut [u32]` backing, no MMIO, no `unsafe`.
//! The kernel side owns the actual DMA-visible buffer (a contiguous
//! physical allocation mapped into the framebuffer VA window); this
//! module only knows how to compute pixel offsets and write pixels.
//!
//! **Why `[u32]` and not `[u8]`**: a byte slice has alignment 1, so LLVM
//! cannot prove a four-byte store is aligned and riscv64gc will not emit
//! an unaligned `sw`. The byte-level version therefore compiled to *four
//! `sb` stores per pixel* no matter how the loop was arranged. A pixel
//! slice makes the store provably aligned and cuts the inner loop from 11
//! instructions per pixel to 3. See `plans/kitsch-v1.md` increment 1.

/// A rectangular region: top-left `(x, y)`, `width` × `height` pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
}

/// A view over a framebuffer's backing pixels. `stride` is the number of
/// **pixels** per row, which may exceed `width` (row padding); pixel ops
/// must always step by `stride`, never assume `width`.
pub struct Framebuffer<'a> {
    pixels: &'a mut [u32],
    width: usize,
    height: usize,
    stride: usize,
}

impl<'a> Framebuffer<'a> {
    /// `stride` is in pixels, not bytes.
    pub fn new(pixels: &'a mut [u32], width: usize, height: usize, stride: usize) -> Self {
        Self { pixels, width, height, stride }
    }

    /// Fill every pixel with `color` (packed `0xRRGGBB`).
    pub fn clear(&mut self, color: u32) {
        let (w, h) = (self.width, self.height);
        self.fill_rect(Rect { x: 0, y: 0, width: w, height: h }, color);
    }

    /// Fill the pixels within `rect` with `color`. Clips silently to
    /// the framebuffer's bounds — a rect that extends past the edge
    /// fills only the in-bounds portion rather than panicking or
    /// wrapping into the next row.
    ///
    /// Fills a **row at a time** via [`slice::fill`]: one bounds check per
    /// row rather than two per pixel, over a slice whose element type is
    /// already the store width.
    pub fn fill_rect(&mut self, rect: Rect, color: u32) {
        let px = pixel_value(color);
        let x_end = (rect.x + rect.width).min(self.width);
        let y_end = (rect.y + rect.height).min(self.height);
        // A rect starting at or past the right edge clips to nothing. Guarded
        // explicitly because the row slice below would otherwise be reversed.
        if x_end <= rect.x {
            return;
        }
        let run = x_end - rect.x;
        for y in rect.y..y_end {
            let start = y * self.stride + rect.x;
            self.pixels[start..start + run].fill(px);
        }
    }
}

/// Pack `0xRRGGBB` into an XRGB8888 pixel: a fixed `0xff` alpha/pad byte in
/// the top octet, then red, green, blue. On a little-endian target — which
/// every target this runs on is — that lands in memory as `[B, G, R, 0xff]`,
/// the byte order a real display expects, independent of `fw_cfg`'s
/// big-endian *wire* format for the mode config.
fn pixel_value(color: u32) -> u32 {
    0xff00_0000 | (color & 0x00ff_ffff)
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;
    use std::vec;

    /// `stride` in pixels, as the view takes it.
    fn backing(height: usize, stride: usize) -> std::vec::Vec<u32> {
        vec![0u32; stride * height]
    }

    fn pixel_at(pixels: &[u32], stride: usize, x: usize, y: usize) -> u32 {
        pixels[y * stride + x]
    }

    #[test]
    fn clear_fills_every_pixel_with_the_given_color() {
        let (w, h, stride) = (4, 3, 4);
        let mut buf = backing(h, stride);
        let mut fb = Framebuffer::new(&mut buf, w, h, stride);
        fb.clear(0x00_11_22_33 | 0xFF_00_00_00); // ignore alpha byte, care about RGB
        fb.clear(0x11_22_33);

        for y in 0..h {
            for x in 0..w {
                assert_eq!(
                    pixel_at(&buf, stride, x, y),
                    0xff_11_22_33,
                    "pixel ({x},{y}) not cleared"
                );
            }
        }
    }

    #[test]
    fn fill_rect_writes_only_the_given_region() {
        let (w, h, stride) = (8, 8, 8);
        let mut buf = backing(h, stride);
        let mut fb = Framebuffer::new(&mut buf, w, h, stride);
        fb.fill_rect(Rect { x: 2, y: 2, width: 3, height: 2 }, 0xAA_BB_CC);

        // Inside the rect: painted.
        for y in 2..4 {
            for x in 2..5 {
                assert_eq!(pixel_at(&buf, stride, x, y), 0xff_aa_bb_cc);
            }
        }
        // Just outside every edge: untouched (still zero).
        assert_eq!(pixel_at(&buf, stride, 1, 2), 0);
        assert_eq!(pixel_at(&buf, stride, 5, 2), 0);
        assert_eq!(pixel_at(&buf, stride, 2, 1), 0);
        assert_eq!(pixel_at(&buf, stride, 2, 4), 0);
    }

    #[test]
    fn fill_rect_respects_stride_wider_than_width() {
        // stride (8 px) leaves 2 px of row padding beyond width (6) — a bug
        // that computed offsets from width instead of stride would write into
        // the padding and corrupt the next row.
        let (w, h, stride) = (6, 4, 8);
        let mut buf = backing(h, stride);
        let mut fb = Framebuffer::new(&mut buf, w, h, stride);
        fb.fill_rect(Rect { x: 0, y: 0, width: w, height: 1 }, 0xFF_FF_FF);

        // Row 0's pixels are painted...
        for x in 0..w {
            assert_eq!(pixel_at(&buf, stride, x, 0), 0xff_ff_ff_ff);
        }
        // ...but row 0's padding pixels and all of row 1 stay untouched.
        assert_eq!(&buf[w..stride], vec![0u32; stride - w].as_slice());
        assert_eq!(&buf[stride..stride * 2], vec![0u32; stride].as_slice());
    }

    #[test]
    fn fill_rect_paints_a_partial_row_without_touching_margin_or_padding() {
        // The combination the other tests miss: a rect that starts part-way
        // into a row *and* a stride with padding. A row-at-a-time fill that
        // slices from the row start, or one that runs to `stride` instead of
        // `x_end`, passes both narrower tests and fails this one.
        let (w, h, stride) = (6, 2, 8);
        let mut buf = backing(h, stride);
        let mut fb = Framebuffer::new(&mut buf, w, h, stride);
        fb.fill_rect(Rect { x: 2, y: 0, width: 3, height: 1 }, 0x11_22_33);

        for x in 2..5 {
            assert_eq!(pixel_at(&buf, stride, x, 0), 0xff_11_22_33);
        }
        // Left margin (x 0..2) untouched.
        assert_eq!(&buf[0..2], &[0u32; 2]);
        // Right of the rect through the end of the padded row untouched.
        assert_eq!(&buf[5..stride], vec![0u32; stride - 5].as_slice());
        // Next row entirely untouched.
        assert_eq!(&buf[stride..stride * 2], vec![0u32; stride].as_slice());
    }

    #[test]
    fn fill_rect_with_an_empty_rect_writes_nothing() {
        // Degenerate sizes must stay no-ops, not panic and not fill a row.
        let (w, h, stride) = (4, 4, 4);
        for rect in [
            Rect { x: 1, y: 1, width: 0, height: 2 },
            Rect { x: 1, y: 1, width: 2, height: 0 },
            Rect { x: 1, y: 1, width: 0, height: 0 },
        ] {
            let mut buf = backing(h, stride);
            let mut fb = Framebuffer::new(&mut buf, w, h, stride);
            fb.fill_rect(rect, 0xFF_FF_FF);
            assert_eq!(buf, vec![0u32; stride * h], "{rect:?} should write nothing");
        }
    }

    #[test]
    fn fill_rect_clips_to_framebuffer_bounds_instead_of_panicking() {
        let (w, h, stride) = (4, 4, 4);
        let mut buf = backing(h, stride);
        let mut fb = Framebuffer::new(&mut buf, w, h, stride);
        // Rect extends 3 pixels past both the right and bottom edges.
        fb.fill_rect(Rect { x: 2, y: 2, width: 5, height: 5 }, 0x00_FF_00);

        // In-bounds portion painted.
        assert_eq!(pixel_at(&buf, stride, 3, 3), 0xff_00_ff_00);
        // Nothing panicked, and the buffer is exactly its declared size.
        assert_eq!(buf.len(), stride * h);
    }

    #[test]
    fn fill_rect_entirely_out_of_bounds_writes_nothing() {
        let (w, h, stride) = (4, 4, 4);
        let mut buf = backing(h, stride);
        let mut fb = Framebuffer::new(&mut buf, w, h, stride);
        fb.fill_rect(Rect { x: 10, y: 10, width: 2, height: 2 }, 0xFF_00_00);
        assert_eq!(buf, vec![0u32; stride * h]);
    }
}
