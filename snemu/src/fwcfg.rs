//! A minimal `fw_cfg` device: the legacy selector+data directory read (this
//! module) plus the DMA select+write path (added in a later step of
//! `plans/snemu-ramfb-model.md`).
//!
//! snemu deliberately reimplements the wire format independently of
//! `kernel_core::fwcfg` rather than sharing it — same "independent oracle"
//! reasoning as `virtio.rs`. The kernel's host-tested
//! `kernel-core/src/fwcfg.rs` is the spec this must satisfy; nothing here
//! shares code with it, only the wire layout.

/// Selector key for the file directory (`FW_CFG_FILE_DIR`), fixed by the
/// `fw_cfg` spec — matches `kernel_core::fwcfg::SELECTOR_FILE_DIR`.
const SELECTOR_FILE_DIR: u16 = 0x19;

/// Size in bytes of one file-directory entry on the wire.
const ENTRY_SIZE: usize = 64;
const NAME_LEN: usize = 56;

/// This milestone serves exactly one file. Its select key is arbitrary (the
/// kernel driver discovers it from the directory, never hardcodes it) but
/// fixed here for simplicity — a general-purpose file registry is more
/// machinery than one file needs.
const RAMFB_SELECT_KEY: u16 = 0x42;
const RAMFB_NAME: &[u8] = b"etc/ramfb";
/// Reported directory-entry size for `etc/ramfb` — not consumed by the
/// kernel driver (it only uses the select key), kept spec-shaped by matching
/// the real `RAMFBCfg` struct size.
const RAMFB_REPORTED_SIZE: u32 = 28;

/// The `fw_cfg` device's legacy (non-DMA) interface: a selector register
/// picks an item, then sequential data-register reads return its bytes.
#[derive(Clone)]
pub(crate) struct Fwcfg {
    /// The currently selected item, if any. `None` until the first
    /// `write_selector` — reads before that return `0` (matches the old
    /// no-op stub's safe-degrade default for anything un-modeled).
    selected: Option<u16>,
    /// Read cursor into the selected item's byte content.
    cursor: usize,
}

impl Fwcfg {
    pub(crate) fn new() -> Self {
        Self { selected: None, cursor: 0 }
    }

    /// Select an item by key (the selector register, offset `0x08`). Resets
    /// the read cursor — re-selecting the same key starts a fresh read.
    pub(crate) fn write_selector(&mut self, key: u16) {
        self.selected = Some(key);
        self.cursor = 0;
    }

    /// Read the next byte of the selected item's content (the data
    /// register, offset `0x00`), advancing the cursor. Returns `0` past the
    /// item's end or with nothing selected — never panics on overrun.
    pub(crate) fn read_data_byte(&mut self) -> u8 {
        let Some(key) = self.selected else { return 0 };
        let byte = Self::item_bytes(key).get(self.cursor).copied().unwrap_or(0);
        self.cursor += 1;
        byte
    }

    /// The byte content of item `key`: the one-entry file directory for
    /// `SELECTOR_FILE_DIR`, empty for anything else (no other legacy items
    /// exist this milestone).
    fn item_bytes(key: u16) -> Vec<u8> {
        if key == SELECTOR_FILE_DIR {
            directory_bytes()
        } else {
            Vec::new()
        }
    }
}

/// Build the one-entry directory blob: `[count: u32 BE][entry]`, entry =
/// `[size: u32 BE][select: u16 BE][reserved: u16][name: [u8; 56]]`. Matches
/// `kernel-core/src/fwcfg.rs::find_file`'s parse exactly (host-tested there).
fn directory_bytes() -> Vec<u8> {
    let mut buf = Vec::with_capacity(4 + ENTRY_SIZE);
    buf.extend_from_slice(&1u32.to_be_bytes()); // count = 1
    buf.extend_from_slice(&RAMFB_REPORTED_SIZE.to_be_bytes());
    buf.extend_from_slice(&RAMFB_SELECT_KEY.to_be_bytes());
    buf.extend_from_slice(&[0u8; 2]); // reserved
    let mut name_field = [0u8; NAME_LEN];
    name_field[..RAMFB_NAME.len()].copy_from_slice(RAMFB_NAME);
    buf.extend_from_slice(&name_field);
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Read `n` bytes off the device via the legacy sequential-read protocol.
    fn read_n(dev: &mut Fwcfg, n: usize) -> Vec<u8> {
        (0..n).map(|_| dev.read_data_byte()).collect()
    }

    #[test]
    fn selecting_file_dir_then_reading_reproduces_the_directory_blob() {
        let mut dev = Fwcfg::new();
        dev.write_selector(SELECTOR_FILE_DIR);
        let blob = read_n(&mut dev, 4 + ENTRY_SIZE);

        assert_eq!(&blob[0..4], &1u32.to_be_bytes(), "count header");
        assert_eq!(&blob[4..8], &RAMFB_REPORTED_SIZE.to_be_bytes(), "entry size");
        assert_eq!(&blob[8..10], &RAMFB_SELECT_KEY.to_be_bytes(), "select key");
        assert_eq!(&blob[10..12], &[0, 0], "reserved");
        let name_field = &blob[12..12 + NAME_LEN];
        assert_eq!(&name_field[..RAMFB_NAME.len()], RAMFB_NAME, "name");
        assert!(name_field[RAMFB_NAME.len()..].iter().all(|&b| b == 0), "name NUL-padded");
    }

    #[test]
    fn selecting_an_unknown_key_reads_as_empty() {
        let mut dev = Fwcfg::new();
        dev.write_selector(0x9999);
        assert_eq!(read_n(&mut dev, 8), vec![0u8; 8]);
    }

    #[test]
    fn reading_before_any_selector_write_returns_zero() {
        let mut dev = Fwcfg::new();
        assert_eq!(read_n(&mut dev, 4), vec![0u8; 4]);
    }

    #[test]
    fn reading_past_the_directory_end_returns_zero_not_panic() {
        let mut dev = Fwcfg::new();
        dev.write_selector(SELECTOR_FILE_DIR);
        let blob = read_n(&mut dev, 4 + ENTRY_SIZE + 16); // 16 bytes past the end
        assert!(blob[4 + ENTRY_SIZE..].iter().all(|&b| b == 0));
    }

    #[test]
    fn reselecting_resets_the_read_cursor() {
        let mut dev = Fwcfg::new();
        dev.write_selector(SELECTOR_FILE_DIR);
        let first_four = read_n(&mut dev, 4);
        dev.write_selector(SELECTOR_FILE_DIR); // re-select the same key
        let again = read_n(&mut dev, 4);
        assert_eq!(first_four, again, "re-selecting must restart the cursor at byte 0");
    }
}
