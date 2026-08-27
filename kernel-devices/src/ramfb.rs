//! `ramfb` display configuration — the config blob written via `fw_cfg`
//! DMA to hand QEMU a guest-allocated framebuffer. Pure layout: no
//! MMIO, no `unsafe`. Fields are big-endian on the wire, per the
//! `fw_cfg` convention (RISC-V is little-endian, so this is exactly
//! the kind of boundary that hides bugs silently).

/// DRM fourcc for XRGB8888 (`fourcc_code('X','R','2','4')`), the pixel
/// format this milestone hardcodes.
pub const FOURCC_XRGB8888: u32 = 0x3432_5258;

/// How many frames back a mode of `height` rows at `stride` bytes each.
///
/// Rounds **up**: a mode whose bytes don't divide evenly into frames still needs
/// a whole final frame, and truncating instead hands the device a buffer shorter
/// than the geometry it was told to scan out — a DMA write past the allocation,
/// silent until it corrupts whatever follows.
pub const fn frames_needed(height: usize, stride: usize, frame_size: usize) -> usize {
    (height * stride).div_ceil(frame_size)
}

/// The `RAMFBCfg` struct QEMU's `ramfb` device expects, written via
/// `etc/ramfb`'s DMA select key. 28 bytes on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RamfbCfg {
    pub addr: u64,
    pub fourcc: u32,
    pub flags: u32,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
}

impl RamfbCfg {
    /// Serialize to the exact 28 big-endian bytes QEMU's `ramfb` device
    /// reads: `addr(8) fourcc(4) flags(4) width(4) height(4) stride(4)`.
    pub fn to_bytes(self) -> [u8; 28] {
        let mut buf = [0u8; 28];
        buf[0..8].copy_from_slice(&self.addr.to_be_bytes());
        buf[8..12].copy_from_slice(&self.fourcc.to_be_bytes());
        buf[12..16].copy_from_slice(&self.flags.to_be_bytes());
        buf[16..20].copy_from_slice(&self.width.to_be_bytes());
        buf[20..24].copy_from_slice(&self.height.to_be_bytes());
        buf[24..28].copy_from_slice(&self.stride.to_be_bytes());
        buf
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mode_whose_bytes_do_not_fill_whole_pages_is_still_fully_backed() {
        // 960x540x4 = 2,073,600 bytes = 506.25 pages. Truncating division backs
        // 506 pages and leaves the device DMA-ing 1 KiB past the allocation.
        assert_eq!(frames_needed(540, 960 * 4, 4096), 507);
    }

    #[test]
    fn a_mode_that_exactly_fills_pages_allocates_no_spare() {
        // 1280x720x4 = 3,686,400 bytes = exactly 900 pages.
        assert_eq!(frames_needed(720, 1280 * 4, 4096), 900);
    }

    #[test]
    fn serializes_every_field_to_exact_big_endian_bytes() {
        // Distinct, non-palindromic bytes per field so a transposed field or a
        // wrong byte range is caught, not masked by a repeated pattern.
        let cfg = RamfbCfg {
            addr: 0x0102_0304_0506_0708,
            fourcc: FOURCC_XRGB8888,
            flags: 0x1112_1314,
            width: 1024,
            height: 768,
            stride: 4096,
        };
        let expected: [u8; 28] = [
            0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, // addr
            0x34, 0x32, 0x52, 0x58, // fourcc (XRGB8888 BE)
            0x11, 0x12, 0x13, 0x14, // flags
            0x00, 0x00, 0x04, 0x00, // width = 1024
            0x00, 0x00, 0x03, 0x00, // height = 768
            0x00, 0x00, 0x10, 0x00, // stride = 4096
        ];
        assert_eq!(cfg.to_bytes(), expected);
    }

    #[test]
    fn addr_is_big_endian_not_little_endian() {
        let cfg = RamfbCfg {
            addr: 0x0001_0203_0405_0607,
            fourcc: 0,
            flags: 0,
            width: 0,
            height: 0,
            stride: 0,
        };
        let bytes = cfg.to_bytes();
        assert_eq!(&bytes[0..8], &[0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07]);
        assert_ne!(&bytes[0..8], &cfg.addr.to_le_bytes());
    }

    #[test]
    fn width_and_height_are_big_endian_not_little_endian() {
        let cfg = RamfbCfg {
            addr: 0,
            fourcc: 0,
            flags: 0,
            width: 0x0000_0102,
            height: 0x0000_0304,
            stride: 0,
        };
        let bytes = cfg.to_bytes();
        assert_eq!(&bytes[16..20], &[0x00, 0x00, 0x01, 0x02]);
        assert_eq!(&bytes[20..24], &[0x00, 0x00, 0x03, 0x04]);
    }
}
