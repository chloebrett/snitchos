//! A minimal `fw_cfg` device: the legacy selector+data directory read (this
//! module) plus the DMA select+write path (added in a later step of
//! `plans/snemu-ramfb-model.md`).
//!
//! snemu deliberately reimplements the wire format independently of
//! `kernel_core::fwcfg` rather than sharing it — same "independent oracle"
//! reasoning as `virtio.rs`. The kernel's host-tested
//! `kernel-core/src/fwcfg.rs` is the spec this must satisfy; nothing here
//! shares code with it, only the wire layout.

use std::sync::atomic::{AtomicI32, AtomicUsize, Ordering};

use crate::mem::Memory;

/// Selector key for the file directory (`FW_CFG_FILE_DIR`), fixed by the
/// `fw_cfg` spec — matches `kernel_core::fwcfg::SELECTOR_FILE_DIR`.
const SELECTOR_FILE_DIR: u16 = 0x19;

/// DMA control bits — matches `kernel_core::fwcfg::{DMA_CTL_SELECT,
/// DMA_CTL_WRITE, DMA_CTL_ERROR}` (real spec values, low to high:
/// `ERROR=0x01, READ=0x02, SKIP=0x04, SELECT=0x08, WRITE=0x10`).
const DMA_CTL_SELECT: u32 = 0x08;
const DMA_CTL_WRITE: u32 = 0x10;
const DMA_CTL_ERROR: u32 = 0x01;

/// The captured `RAMFBCfg` fields from a successful DMA write — this
/// milestone's only writable file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RamfbCfg {
    pub(crate) addr: u64,
    pub(crate) fourcc: u32,
    pub(crate) flags: u32,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) stride: u32,
}

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
///
/// `selected`/`cursor` are atomics, not plain fields: reading the data
/// register has a hardware side effect (the cursor advances), but `Bus`'s
/// `read_u8` — which every load instruction goes through — is `&self`, not
/// `&mut self` (reads never mutate for every other device on the bus, which
/// have no such state). Interior mutability is the minimal-blast-radius fix;
/// widening `Bus::read_u8` to `&mut self` would cascade through the whole
/// CPU/mmu load path for one device's quirk. Atomics rather than `Cell`
/// specifically because `Machine`/`Bus` must stay `Send + Sync` — the
/// `snemu-itest` parallel worker pool shares booted machines across threads
/// (`Arc<Mutex<HashMap<_, Machine>>>`), and `Cell` isn't `Sync`. No real
/// contention ever happens (each thread works its own `Clone`d `Machine`);
/// `Ordering::Relaxed` throughout reflects that this is a type-system
/// requirement, not real synchronization.
pub(crate) struct Fwcfg {
    /// The currently selected item, if any. `-1` (not `Option<u16>` — atomics
    /// don't hold `Option`) means none; reads before the first
    /// `write_selector` return `0`, matching the old no-op stub's
    /// safe-degrade default for anything un-modeled.
    selected: AtomicI32,
    /// Read cursor into the selected item's byte content.
    cursor: AtomicUsize,
    /// The captured config from a successful `etc/ramfb` DMA write, if any.
    ramfb_cfg: Option<RamfbCfg>,
    /// The DMA descriptor's physical address, assembled from separate
    /// high/low 32-bit register writes (mirrors `virtio::Queue`'s
    /// `desc`/`avail`/`used` assembly via `put_low`/`put_high`).
    dma_addr: u64,
}

/// Atomics don't derive `Clone` (cloning one means loading its current value
/// into a fresh independent atomic, not automatically derivable) — the
/// fork/snapshot-tree machinery (`Machine`/`Bus`'s `#[derive(Clone)]`) needs
/// every field to implement `Clone` by *some* means, so this is that means.
impl Clone for Fwcfg {
    fn clone(&self) -> Self {
        Self {
            selected: AtomicI32::new(self.selected.load(Ordering::Relaxed)),
            cursor: AtomicUsize::new(self.cursor.load(Ordering::Relaxed)),
            ramfb_cfg: self.ramfb_cfg,
            dma_addr: self.dma_addr,
        }
    }
}

impl Fwcfg {
    pub(crate) fn new() -> Self {
        Self {
            selected: AtomicI32::new(-1),
            cursor: AtomicUsize::new(0),
            ramfb_cfg: None,
            dma_addr: 0,
        }
    }

    /// The captured `RAMFBCfg`, if a valid `etc/ramfb` DMA write has
    /// completed.
    pub(crate) fn ramfb_cfg(&self) -> Option<RamfbCfg> {
        self.ramfb_cfg
    }

    /// Complete a DMA select+write transfer: read the 16-byte descriptor at
    /// `desc_pa`, validate it's a well-formed write to `etc/ramfb`'s select
    /// key, capture the `RAMFBCfg` payload it points at, and write the
    /// completion status back into the descriptor's `control` field —
    /// `0` on success, [`DMA_CTL_ERROR`] otherwise. Mirrors `Virtio::
    /// service_tx`'s split: the bus detects the trigger register write and
    /// calls this; the RAM-touching work happens here, not at the register
    /// write itself.
    ///
    /// All descriptor/register fields are big-endian on the wire regardless
    /// of guest endianness (the `fw_cfg` convention) — `Memory` decodes
    /// raw bytes as little-endian (this is a RISC-V guest), so every field
    /// read here goes through `u32::from_be`/`u64::from_be` to recover the
    /// logical value, and every field written goes through `.to_be()` to
    /// place the correct wire bytes.
    pub(crate) fn complete_dma(&mut self, ram: &mut Memory, desc_pa: u64) {
        let control = u32::from_be(ram.read_u32(desc_pa).unwrap_or(0));
        let length = u32::from_be(ram.read_u32(desc_pa + 4).unwrap_or(0));
        let address = u64::from_be(ram.read_u64(desc_pa + 8).unwrap_or(0));

        let select_key = (control >> 16) as u16;
        let valid = control & DMA_CTL_SELECT != 0
            && control & DMA_CTL_WRITE != 0
            && select_key == RAMFB_SELECT_KEY
            && length == RAMFB_REPORTED_SIZE;

        let status = if valid {
            self.ramfb_cfg = Some(read_ramfb_cfg(ram, address));
            0
        } else {
            DMA_CTL_ERROR
        };
        let _ = ram.write_u32(desc_pa, status.to_be());
    }

    /// Stage the high half of the DMA descriptor's physical address
    /// (register offset `0x10`). Takes an already wire-decoded logical
    /// value — the bus is responsible for `u32::from_be` before calling,
    /// same split as `write_selector`.
    pub(crate) fn write_dma_addr_high(&mut self, value: u32) {
        put_high(&mut self.dma_addr, value);
    }

    /// Stage the low half (register offset `0x14`) and return the
    /// assembled 64-bit descriptor address. This is the DMA **trigger** —
    /// the real device (and the kernel driver's `write_file`, which writes
    /// high then low) treats the low-half write as "go"; the bus calls
    /// [`Self::complete_dma`] with the returned address immediately after.
    pub(crate) fn write_dma_addr_low(&mut self, value: u32) -> u64 {
        put_low(&mut self.dma_addr, value);
        self.dma_addr
    }

    /// Select an item by key (the selector register, offset `0x08`). Resets
    /// the read cursor — re-selecting the same key starts a fresh read.
    pub(crate) fn write_selector(&mut self, key: u16) {
        self.selected.store(i32::from(key), Ordering::Relaxed);
        self.cursor.store(0, Ordering::Relaxed);
    }

    /// Read the next byte of the selected item's content (the data
    /// register, offset `0x00`), advancing the cursor. Returns `0` past the
    /// item's end or with nothing selected — never panics on overrun.
    /// `&self`, not `&mut self` — see the struct doc for why the state is
    /// atomic.
    pub(crate) fn read_data_byte(&self) -> u8 {
        let selected = self.selected.load(Ordering::Relaxed);
        let Ok(key) = u16::try_from(selected) else { return 0 }; // -1 = none selected
        let cursor = self.cursor.load(Ordering::Relaxed);
        let byte = Self::item_bytes(key).get(cursor).copied().unwrap_or(0);
        self.cursor.store(cursor + 1, Ordering::Relaxed);
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

/// Set the low / high 32-bit half of a 64-bit register slot. Matches
/// `virtio.rs`'s helpers of the same name (duplicated, not shared — each
/// device stays self-contained, same reasoning as the wire-format
/// reimplementation this module's header comment describes).
fn put_low(slot: &mut u64, value: u32) {
    *slot = (*slot & !0xFFFF_FFFF) | u64::from(value);
}
fn put_high(slot: &mut u64, value: u32) {
    *slot = (*slot & 0xFFFF_FFFF) | (u64::from(value) << 32);
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

/// Decode a 28-byte `RAMFBCfg` from guest RAM at `address`, big-endian —
/// matches `kernel-core/src/ramfb.rs::RamfbCfg::to_bytes`'s layout exactly.
/// Unreadable bytes degrade to `0`, same as every other RAM read in this
/// module — `complete_dma` has already validated the length before calling.
fn read_ramfb_cfg(ram: &Memory, address: u64) -> RamfbCfg {
    RamfbCfg {
        addr: u64::from_be(ram.read_u64(address).unwrap_or(0)),
        fourcc: u32::from_be(ram.read_u32(address + 8).unwrap_or(0)),
        flags: u32::from_be(ram.read_u32(address + 12).unwrap_or(0)),
        width: u32::from_be(ram.read_u32(address + 16).unwrap_or(0)),
        height: u32::from_be(ram.read_u32(address + 20).unwrap_or(0)),
        stride: u32::from_be(ram.read_u32(address + 24).unwrap_or(0)),
    }
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

    use crate::mem::{Memory, RAM_BASE};

    const DESC_PA: u64 = RAM_BASE + 0x1000;
    const PAYLOAD_PA: u64 = RAM_BASE + 0x2000;

    /// Stage a valid select+write descriptor at `DESC_PA` pointing at a
    /// 28-byte `RAMFBCfg` payload at `PAYLOAD_PA`, matching exactly what
    /// `kernel/src/device/fwcfg.rs::write_file` stages for a real write.
    fn stage_valid_write(mem: &mut Memory, cfg_bytes: &[u8; 28]) {
        let control = (u32::from(RAMFB_SELECT_KEY) << 16) | DMA_CTL_SELECT | DMA_CTL_WRITE;
        mem.write_u32(DESC_PA, control.to_be()).unwrap();
        mem.write_u32(DESC_PA + 4, RAMFB_REPORTED_SIZE.to_be()).unwrap();
        mem.write_u64(DESC_PA + 8, PAYLOAD_PA.to_be()).unwrap();
        for (i, &b) in cfg_bytes.iter().enumerate() {
            mem.write_u8(PAYLOAD_PA + i as u64, b).unwrap();
        }
    }

    /// A `RAMFBCfg`'s 28 big-endian bytes, matching
    /// `kernel-core/src/ramfb.rs::RamfbCfg::to_bytes` exactly — the fixture
    /// every test below stages as the DMA payload.
    fn sample_cfg_bytes() -> [u8; 28] {
        let mut buf = [0u8; 28];
        buf[0..8].copy_from_slice(&0x8000_1000u64.to_be_bytes()); // addr
        buf[8..12].copy_from_slice(&0x3432_5258u32.to_be_bytes()); // fourcc (XRGB8888)
        buf[12..16].copy_from_slice(&0u32.to_be_bytes()); // flags
        buf[16..20].copy_from_slice(&1024u32.to_be_bytes()); // width
        buf[20..24].copy_from_slice(&768u32.to_be_bytes()); // height
        buf[24..28].copy_from_slice(&4096u32.to_be_bytes()); // stride
        buf
    }

    #[test]
    fn a_valid_select_and_write_captures_the_config_and_clears_control() {
        let mut mem = Memory::new(0x10000);
        stage_valid_write(&mut mem, &sample_cfg_bytes());
        let mut dev = Fwcfg::new();

        dev.complete_dma(&mut mem, DESC_PA);

        assert_eq!(
            dev.ramfb_cfg(),
            Some(RamfbCfg {
                addr: 0x8000_1000,
                fourcc: 0x3432_5258,
                flags: 0,
                width: 1024,
                height: 768,
                stride: 4096,
            })
        );
        assert_eq!(mem.read_u32(DESC_PA).unwrap(), 0, "control cleared on success");
    }

    #[test]
    fn an_unknown_select_key_reports_error_and_captures_nothing() {
        let mut mem = Memory::new(0x10000);
        stage_valid_write(&mut mem, &sample_cfg_bytes());
        // Corrupt just the select key half of control, leaving SELECT|WRITE set.
        let bad_control = (0x9999u32 << 16) | DMA_CTL_SELECT | DMA_CTL_WRITE;
        mem.write_u32(DESC_PA, bad_control.to_be()).unwrap();
        let mut dev = Fwcfg::new();

        dev.complete_dma(&mut mem, DESC_PA);

        assert_eq!(dev.ramfb_cfg(), None);
        assert_eq!(mem.read_u32(DESC_PA).unwrap().to_be(), DMA_CTL_ERROR);
    }

    #[test]
    fn a_read_request_without_the_write_bit_is_rejected() {
        let mut mem = Memory::new(0x10000);
        stage_valid_write(&mut mem, &sample_cfg_bytes());
        let read_control = (u32::from(RAMFB_SELECT_KEY) << 16) | DMA_CTL_SELECT; // no WRITE
        mem.write_u32(DESC_PA, read_control.to_be()).unwrap();
        let mut dev = Fwcfg::new();

        dev.complete_dma(&mut mem, DESC_PA);

        assert_eq!(dev.ramfb_cfg(), None);
        assert_eq!(mem.read_u32(DESC_PA).unwrap().to_be(), DMA_CTL_ERROR);
    }

    #[test]
    fn a_mismatched_length_is_rejected() {
        let mut mem = Memory::new(0x10000);
        stage_valid_write(&mut mem, &sample_cfg_bytes());
        mem.write_u32(DESC_PA + 4, 4u32.to_be()).unwrap(); // wrong length
        let mut dev = Fwcfg::new();

        dev.complete_dma(&mut mem, DESC_PA);

        assert_eq!(dev.ramfb_cfg(), None);
        assert_eq!(mem.read_u32(DESC_PA).unwrap().to_be(), DMA_CTL_ERROR);
    }

    #[test]
    fn ramfb_cfg_is_none_until_a_write_completes() {
        let dev = Fwcfg::new();
        assert_eq!(dev.ramfb_cfg(), None);
    }

    #[test]
    fn dma_addr_high_then_low_assembles_the_64_bit_descriptor_address() {
        let mut dev = Fwcfg::new();
        dev.write_dma_addr_high(0x0001_0203);
        let assembled = dev.write_dma_addr_low(0x8000_2000);
        assert_eq!(assembled, 0x0001_0203_8000_2000);
    }

    #[test]
    fn writing_the_low_half_again_reassembles_with_the_last_staged_high_half() {
        // Two back-to-back select+write sequences (the real driver's actual
        // pattern, once per DMA op) must each assemble correctly, not leak
        // the previous op's high half in some stale way.
        let mut dev = Fwcfg::new();
        dev.write_dma_addr_high(0x0000_0000);
        assert_eq!(dev.write_dma_addr_low(0x8000_1000), 0x8000_1000);
        dev.write_dma_addr_high(0x0000_0001);
        assert_eq!(dev.write_dma_addr_low(0x8000_2000), 0x1_8000_2000);
    }
}
