//! Pixel operations over a raw XRGB8888 framebuffer. Pure: a view
//! over a caller-provided `&mut [u8]` backing, no MMIO, no `unsafe`.
//! The kernel side owns the actual DMA-visible buffer (a contiguous
//! physical allocation mapped into the framebuffer VA window); this
//! module only knows how to compute pixel offsets and write bytes.

/// A rectangular region: top-left `(x, y)`, `width` × `height` pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
}

/// A view over a framebuffer's backing bytes. `stride` is the number
/// of bytes per row, which may exceed `width * 4` (row padding); pixel
/// ops must always step by `stride`, never assume `width * 4`.
pub struct Framebuffer<'a> {
    bytes: &'a mut [u8],
    width: usize,
    height: usize,
    stride: usize,
}

impl<'a> Framebuffer<'a> {
    pub fn new(bytes: &'a mut [u8], width: usize, height: usize, stride: usize) -> Self {
        Self { bytes, width, height, stride }
    }

    /// Fill every pixel with `color` (packed `0xRRGGBB`, stored as
    /// `[B, G, R, 0xff]` little-endian XRGB8888 bytes per pixel — the
    /// byte order a real display expects, independent of `fw_cfg`'s
    /// big-endian wire format).
    pub fn clear(&mut self, color: u32) {
        let (w, h) = (self.width, self.height);
        self.fill_rect(Rect { x: 0, y: 0, width: w, height: h }, color);
    }

    /// Fill the pixels within `rect` with `color`. Clips silently to
    /// the framebuffer's bounds — a rect that extends past the edge
    /// fills only the in-bounds portion rather than panicking or
    /// wrapping into the next row.
    pub fn fill_rect(&mut self, rect: Rect, color: u32) {
        let px = color_to_bytes(color);
        let x_end = (rect.x + rect.width).min(self.width);
        let y_end = (rect.y + rect.height).min(self.height);
        for y in rect.y..y_end {
            let row_start = y * self.stride;
            for x in rect.x..x_end {
                let offset = row_start + x * 4;
                self.bytes[offset..offset + 4].copy_from_slice(&px);
            }
        }
    }
}

/// Pack `0xRRGGBB` into the four XRGB8888 bytes written per pixel:
/// blue, green, red, then a fixed `0xff` alpha/pad byte.
fn color_to_bytes(color: u32) -> [u8; 4] {
    let [_, r, g, b] = color.to_be_bytes();
    [b, g, r, 0xff]
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;
    use std::vec;

    fn backing(height: usize, stride: usize) -> std::vec::Vec<u8> {
        vec![0u8; stride * height]
    }

    fn pixel_at(bytes: &[u8], stride: usize, x: usize, y: usize) -> [u8; 4] {
        let offset = y * stride + x * 4;
        bytes[offset..offset + 4].try_into().unwrap()
    }

    #[test]
    fn clear_fills_every_pixel_with_the_given_color() {
        let (w, h, stride) = (4, 3, 16);
        let mut buf = backing(h, stride);
        let mut fb = Framebuffer::new(&mut buf, w, h, stride);
        fb.clear(0x00_11_22_33 | 0xFF_00_00_00); // ignore alpha byte, care about RGB
        fb.clear(0x11_22_33);

        for y in 0..h {
            for x in 0..w {
                assert_eq!(
                    pixel_at(&buf, stride, x, y),
                    [0x33, 0x22, 0x11, 0xff],
                    "pixel ({x},{y}) not cleared"
                );
            }
        }
    }

    #[test]
    fn fill_rect_writes_only_the_given_region() {
        let (w, h, stride) = (8, 8, 32);
        let mut buf = backing(h, stride);
        let mut fb = Framebuffer::new(&mut buf, w, h, stride);
        fb.fill_rect(Rect { x: 2, y: 2, width: 3, height: 2 }, 0xAA_BB_CC);

        // Inside the rect: painted.
        for y in 2..4 {
            for x in 2..5 {
                assert_eq!(pixel_at(&buf, stride, x, y), [0xcc, 0xbb, 0xaa, 0xff]);
            }
        }
        // Just outside every edge: untouched (still zero).
        assert_eq!(pixel_at(&buf, stride, 1, 2), [0, 0, 0, 0]);
        assert_eq!(pixel_at(&buf, stride, 5, 2), [0, 0, 0, 0]);
        assert_eq!(pixel_at(&buf, stride, 2, 1), [0, 0, 0, 0]);
        assert_eq!(pixel_at(&buf, stride, 2, 4), [0, 0, 0, 0]);
    }

    #[test]
    fn fill_rect_respects_stride_wider_than_width_times_four() {
        // stride (32) leaves 8 bytes of row padding beyond width*4 (24) —
        // a bug that computed offsets from width instead of stride would
        // write into the padding and corrupt the next row.
        let (w, h, stride) = (6, 4, 32);
        let mut buf = backing(h, stride);
        let mut fb = Framebuffer::new(&mut buf, w, h, stride);
        fb.fill_rect(Rect { x: 0, y: 0, width: w, height: 1 }, 0xFF_FF_FF);

        // Row 0's pixels are painted...
        for x in 0..w {
            assert_eq!(pixel_at(&buf, stride, x, 0), [0xff, 0xff, 0xff, 0xff]);
        }
        // ...but row 0's padding bytes and all of row 1 stay untouched.
        assert_eq!(&buf[w * 4..stride], vec![0u8; stride - w * 4].as_slice());
        assert_eq!(&buf[stride..stride * 2], vec![0u8; stride].as_slice());
    }

    #[test]
    fn fill_rect_clips_to_framebuffer_bounds_instead_of_panicking() {
        let (w, h, stride) = (4, 4, 16);
        let mut buf = backing(h, stride);
        let mut fb = Framebuffer::new(&mut buf, w, h, stride);
        // Rect extends 3 pixels past both the right and bottom edges.
        fb.fill_rect(Rect { x: 2, y: 2, width: 5, height: 5 }, 0x00_FF_00);

        // In-bounds portion painted.
        assert_eq!(pixel_at(&buf, stride, 3, 3), [0x00, 0xff, 0x00, 0xff]);
        // Nothing panicked, and the buffer is exactly its declared size.
        assert_eq!(buf.len(), stride * h);
    }

    #[test]
    fn fill_rect_entirely_out_of_bounds_writes_nothing() {
        let (w, h, stride) = (4, 4, 16);
        let mut buf = backing(h, stride);
        let mut fb = Framebuffer::new(&mut buf, w, h, stride);
        fb.fill_rect(Rect { x: 10, y: 10, width: 2, height: 2 }, 0xFF_00_00);
        assert_eq!(buf, vec![0u8; stride * h]);
    }
}
