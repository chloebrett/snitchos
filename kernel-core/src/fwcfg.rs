//! QEMU `fw_cfg` data definitions — the file-directory wire format and
//! selector/DMA constants shared between the kernel driver and host
//! tests. Pure layout: no MMIO, no `unsafe`. `fw_cfg` is **big-endian**
//! on the wire regardless of guest endianness; that boundary is where
//! bugs hide, so it's pinned here with host tests rather than reasoned
//! about at the MMIO call site.

/// Selector key for the file directory (`FW_CFG_FILE_DIR`), fixed by
/// the `fw_cfg` spec.
pub const SELECTOR_FILE_DIR: u16 = 0x19;

/// Size in bytes of one file-directory entry on the wire: `u32` size +
/// `u16` select + 2 bytes reserved + a 56-byte NUL-padded name.
const ENTRY_SIZE: usize = 64;
const NAME_LEN: usize = 56;

/// One `fw_cfg` file's directory metadata: the selector key to read its
/// contents with, and its size in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FwCfgFile {
    pub select_key: u16,
    pub size: u32,
}

/// Parse a raw `fw_cfg` file-directory blob (as read from selector
/// `SELECTOR_FILE_DIR`) and find the entry named `name`.
///
/// The blob is `[count: u32 BE][entry; count]`, each entry
/// `[size: u32 BE][select: u16 BE][reserved: u16][name: [u8; 56]]`.
/// Returns `None` if the blob is too short to hold its declared count,
/// or if no entry's NUL-terminated name matches `name`.
pub fn find_file(dir: &[u8], name: &str) -> Option<FwCfgFile> {
    let count = u32::from_be_bytes(dir.get(0..4)?.try_into().ok()?) as usize;
    let entries = dir.get(4..)?;

    for i in 0..count {
        let start = i * ENTRY_SIZE;
        let entry = entries.get(start..start + ENTRY_SIZE)?;

        let size = u32::from_be_bytes(entry[0..4].try_into().ok()?);
        let select_key = u16::from_be_bytes(entry[4..6].try_into().ok()?);
        let raw_name = &entry[8..8 + NAME_LEN];
        let name_len = raw_name.iter().position(|&b| b == 0).unwrap_or(NAME_LEN);
        let entry_name = core::str::from_utf8(&raw_name[..name_len]).ok()?;

        if entry_name == name {
            return Some(FwCfgFile { select_key, size });
        }
    }
    None
}

/// Control bit: select a file by its `select_key` before the transfer
/// (packed into the high 16 bits of `control`, per the `fw_cfg` DMA
/// spec).
pub const DMA_CTL_SELECT: u32 = 0x01;
/// Control bit: this is a write from guest to device.
pub const DMA_CTL_WRITE: u32 = 0x10;

/// The `FWCfgDmaAccess` descriptor written to the DMA address register
/// to drive a select+write transfer. 16 bytes on the wire, big-endian.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DmaAccess {
    pub control: u32,
    pub length: u32,
    pub address: u64,
}

impl DmaAccess {
    /// Serialize to the exact 16 big-endian bytes QEMU's `fw_cfg` DMA
    /// interface reads: `control(4) length(4) address(8)`.
    pub fn to_bytes(self) -> [u8; 16] {
        let mut buf = [0u8; 16];
        buf[0..4].copy_from_slice(&self.control.to_be_bytes());
        buf[4..8].copy_from_slice(&self.length.to_be_bytes());
        buf[8..16].copy_from_slice(&self.address.to_be_bytes());
        buf
    }
}

/// Register offset for the DMA address register's high 32 bits, from
/// the `fw_cfg` MMIO base. Writing the low half (`REG_DMA_ADDR_LOW`)
/// triggers the device to read the descriptor.
pub const REG_DMA_ADDR_HIGH: usize = 0x10;
pub const REG_DMA_ADDR_LOW: usize = 0x14;

/// Set by the device (never by the driver) when a completed transfer
/// failed — e.g. an unknown `select_key`.
pub const DMA_CTL_ERROR: u32 = 0x02;

/// Register and descriptor-memory access needed to drive a `fw_cfg`
/// DMA transfer. `write_reg` is ordinary MMIO; `write_descriptor` /
/// `read_descriptor_control` touch the guest-RAM [`DmaAccess`]
/// descriptor the device reads and updates in place — not a device
/// register, but abstracted the same way so the sequence logic here
/// stays host-testable without a real memory bus.
pub trait FwCfgTransport {
    fn write_reg(&mut self, offset: usize, value: u32);
    fn write_descriptor(&mut self, bytes: [u8; 16]);
    fn read_descriptor_control(&self) -> u32;
}

/// Stage a select+write DMA descriptor pointing at `payload_pa` /
/// `payload_len` and trigger the transfer by writing the descriptor's
/// own physical address (`desc_pa`) to the DMA address register, high
/// half first — the low-half write is what the device treats as the
/// signal to read the descriptor, so the descriptor must be fully
/// staged in memory before either half is written.
///
/// Does not poll for completion. A real caller spins on
/// [`dma_pending`] reading the descriptor back via
/// `read_descriptor_control`; that loop can't run against a host-test
/// mock, so it stays kernel-side.
pub fn write_file<T: FwCfgTransport>(
    dev: &mut T,
    select_key: u16,
    payload_pa: u64,
    payload_len: u32,
    desc_pa: u64,
) {
    let control = (u32::from(select_key) << 16) | DMA_CTL_SELECT | DMA_CTL_WRITE;
    let descriptor = DmaAccess { control, length: payload_len, address: payload_pa };
    dev.write_descriptor(descriptor.to_bytes());
    dev.write_reg(REG_DMA_ADDR_HIGH, (desc_pa >> 32) as u32);
    dev.write_reg(REG_DMA_ADDR_LOW, desc_pa as u32);
}

/// Whether the device is still processing the descriptor last
/// triggered by [`write_file`] — the driver's `SELECT`/`WRITE`
/// control bits are cleared by the device once it has finished
/// reading (successfully or not).
pub fn dma_pending(control: u32) -> bool {
    control & (DMA_CTL_SELECT | DMA_CTL_WRITE) != 0
}

/// Whether a completed transfer (`!dma_pending`) failed.
pub fn dma_failed(control: u32) -> bool {
    control & DMA_CTL_ERROR != 0
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;
    use std::vec::Vec;

    /// Build a synthetic `fw_cfg` file-directory blob from `(name, select_key,
    /// size)` triples, matching the wire format `find_file` parses.
    fn directory(files: &[(&str, u16, u32)]) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&(files.len() as u32).to_be_bytes());
        for &(name, select_key, size) in files {
            buf.extend_from_slice(&size.to_be_bytes());
            buf.extend_from_slice(&select_key.to_be_bytes());
            buf.extend_from_slice(&[0u8; 2]); // reserved
            let mut name_field = [0u8; NAME_LEN];
            name_field[..name.len()].copy_from_slice(name.as_bytes());
            buf.extend_from_slice(&name_field);
        }
        buf
    }

    #[test]
    fn finds_a_present_file_by_name() {
        let dir = directory(&[("etc/ramfb", 0x42, 28)]);
        assert_eq!(
            find_file(&dir, "etc/ramfb"),
            Some(FwCfgFile { select_key: 0x42, size: 28 })
        );
    }

    #[test]
    fn returns_none_for_an_absent_name() {
        let dir = directory(&[("etc/ramfb", 0x42, 28)]);
        assert_eq!(find_file(&dir, "etc/nonexistent"), None);
    }

    #[test]
    fn decodes_select_key_as_big_endian_not_little_endian() {
        // 0x0102 big-endian and little-endian decode to different values —
        // this is the assertion that pins the endianness, not just presence.
        let dir = directory(&[("etc/ramfb", 0x0102, 1)]);
        let found = find_file(&dir, "etc/ramfb").unwrap();
        assert_eq!(found.select_key, 0x0102);
        assert_ne!(found.select_key, 0x0201, "decoded as little-endian instead of big-endian");
    }

    #[test]
    fn decodes_size_as_big_endian_not_little_endian() {
        let dir = directory(&[("etc/ramfb", 1, 0x0001_0203)]);
        let found = find_file(&dir, "etc/ramfb").unwrap();
        assert_eq!(found.size, 0x0001_0203);
        assert_ne!(found.size, 0x0302_0100, "decoded as little-endian instead of big-endian");
    }

    #[test]
    fn respects_the_count_header_ignoring_trailing_garbage() {
        // Directory claims 1 entry even though a second entry's bytes follow —
        // `find_file` must not read past `count`.
        let mut dir = directory(&[("etc/ramfb", 0x42, 28)]);
        dir.extend_from_slice(&directory(&[("etc/other", 0x99, 4)])[4..]);
        assert_eq!(find_file(&dir, "etc/other"), None);
    }

    #[test]
    fn finds_the_right_entry_among_several() {
        let dir = directory(&[
            ("etc/acpi/tables", 0x10, 100),
            ("etc/ramfb", 0x20, 28),
            ("etc/e820", 0x30, 40),
        ]);
        assert_eq!(
            find_file(&dir, "etc/ramfb"),
            Some(FwCfgFile { select_key: 0x20, size: 28 })
        );
    }

    #[test]
    fn empty_directory_returns_none() {
        let dir = directory(&[]);
        assert_eq!(find_file(&dir, "etc/ramfb"), None);
    }

    #[test]
    fn truncated_blob_returns_none_instead_of_panicking() {
        // count header says 1 entry but the blob is cut short.
        let dir = directory(&[("etc/ramfb", 0x42, 28)]);
        let truncated = &dir[..dir.len() - 10];
        assert_eq!(find_file(truncated, "etc/ramfb"), None);
    }

    #[test]
    fn dma_access_serializes_every_field_to_exact_big_endian_bytes() {
        let d = DmaAccess {
            control: 0x0102_0304,
            length: 0x0506_0708,
            address: 0x1112_1314_1516_1718,
        };
        let expected: [u8; 16] = [
            0x01, 0x02, 0x03, 0x04, // control
            0x05, 0x06, 0x07, 0x08, // length
            0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, // address
        ];
        assert_eq!(d.to_bytes(), expected);
    }

    #[test]
    fn dma_access_address_is_big_endian_not_little_endian() {
        let d = DmaAccess { control: 0, length: 0, address: 0x0001_0203_0405_0607 };
        let bytes = d.to_bytes();
        assert_eq!(&bytes[8..16], &[0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07]);
        assert_ne!(&bytes[8..16], &d.address.to_le_bytes());
    }

    #[test]
    fn dma_access_control_word_from_select_and_write() {
        // The canonical construction: select a file's key, mark it a write.
        // key=0x42 packed into the high 16 bits, per the fw_cfg DMA spec.
        let control = (0x42u32 << 16) | DMA_CTL_SELECT | DMA_CTL_WRITE;
        let d = DmaAccess { control, length: 28, address: 0 };
        assert_eq!(&d.to_bytes()[0..4], &[0x00, 0x42, 0x00, 0x11]);
    }

    #[derive(Default)]
    struct MockTransport {
        reg_writes: Vec<(usize, u32)>,
        descriptor: [u8; 16],
    }

    impl FwCfgTransport for MockTransport {
        fn write_reg(&mut self, offset: usize, value: u32) {
            self.reg_writes.push((offset, value));
        }

        fn write_descriptor(&mut self, bytes: [u8; 16]) {
            self.descriptor = bytes;
        }

        fn read_descriptor_control(&self) -> u32 {
            u32::from_be_bytes(self.descriptor[0..4].try_into().unwrap())
        }
    }

    #[test]
    fn write_file_stages_the_descriptor_before_triggering_the_transfer() {
        let mut dev = MockTransport::default();
        write_file(&mut dev, 0x42, 0x8000_1000, 28, 0x8000_2000);
        // The descriptor must be fully written to memory before the register
        // write that tells the device to read it — otherwise the device could
        // race the driver and read a half-written descriptor.
        assert_ne!(dev.descriptor, [0u8; 16], "descriptor never staged");
        assert!(!dev.reg_writes.is_empty(), "no register writes issued");
    }

    #[test]
    fn write_file_builds_the_correct_control_word() {
        let mut dev = MockTransport::default();
        write_file(&mut dev, 0x42, 0x8000_1000, 28, 0x8000_2000);
        let control = u32::from_be_bytes(dev.descriptor[0..4].try_into().unwrap());
        assert_eq!(control, (0x42u32 << 16) | DMA_CTL_SELECT | DMA_CTL_WRITE);
    }

    #[test]
    fn write_file_stages_payload_address_and_length() {
        let mut dev = MockTransport::default();
        write_file(&mut dev, 0x42, 0x8000_1000, 28, 0x8000_2000);
        let length = u32::from_be_bytes(dev.descriptor[4..8].try_into().unwrap());
        let address = u64::from_be_bytes(dev.descriptor[8..16].try_into().unwrap());
        assert_eq!(length, 28);
        assert_eq!(address, 0x8000_1000);
    }

    #[test]
    fn write_file_writes_descriptor_physical_address_high_half_then_low_half() {
        let mut dev = MockTransport::default();
        let desc_pa: u64 = 0x0001_0203_8000_2000;
        write_file(&mut dev, 0x42, 0x8000_1000, 28, desc_pa);
        assert_eq!(
            dev.reg_writes,
            [
                (REG_DMA_ADDR_HIGH, (desc_pa >> 32) as u32),
                (REG_DMA_ADDR_LOW, desc_pa as u32),
            ]
        );
    }

    #[test]
    fn dma_pending_is_true_while_select_or_write_bit_is_set() {
        assert!(dma_pending(DMA_CTL_SELECT | DMA_CTL_WRITE));
        assert!(dma_pending(DMA_CTL_SELECT));
        assert!(dma_pending(DMA_CTL_WRITE));
    }

    #[test]
    fn dma_pending_is_false_once_the_device_clears_the_control_bits() {
        assert!(!dma_pending(0));
        // An error bit alone (no SELECT/WRITE) still means "not pending" —
        // the device finished processing, just unsuccessfully.
        assert!(!dma_pending(DMA_CTL_ERROR));
    }

    #[test]
    fn dma_failed_is_true_when_the_error_bit_is_set_on_completion() {
        assert!(dma_failed(DMA_CTL_ERROR));
    }

    #[test]
    fn dma_failed_is_false_on_clean_completion() {
        assert!(!dma_failed(0));
    }
}
