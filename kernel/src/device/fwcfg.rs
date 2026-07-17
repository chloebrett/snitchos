//! `fw_cfg` MMIO transport — QEMU's guest-configuration channel.
//!
//! Unlike virtio-mmio, `fw_cfg`'s base is **fixed**, not DTB-discovered:
//! QEMU `virt` always maps it at `0x1010_0000`, inside the same 2 MiB
//! higher-half MMIO region the virtio-mmio slots and UART already
//! share (no new `MmioRegions` entry needed).
//!
//! Owns the volatile register access and two `.bss`-resident staging
//! buffers (same discipline as virtio-console's `TX_STAGING`: DMA
//! descriptors need a `KERNEL_OFFSET`-range VA that `va_to_pa` can
//! translate; heap VAs can't). The sequence logic itself
//! (`kernel_devices::fwcfg`) is pure and host-tested; this module is the
//! thin MMIO/memory adapter plus the legacy (non-DMA) directory read.

use kernel_devices::fwcfg::{self, FwCfgFile, FwCfgTransport, SELECTOR_FILE_DIR};

/// Fixed `fw_cfg` MMIO base on QEMU `virt`.
const BASE_PA: usize = 0x1010_0000;

const REG_DATA: usize = 0x00;
const REG_SELECTOR: usize = 0x08;

fn base() -> usize {
    BASE_PA + crate::mmu::KERNEL_OFFSET
}

/// Read one byte from the legacy data register. Sequential reads
/// after selecting an item return that item's successive bytes.
///
/// # Safety
///
/// `mmu::enable` must have run (higher-half MMIO must be live).
unsafe fn read_data_byte() -> u8 {
    let addr = (base() + REG_DATA) as *const u8;
    unsafe { addr.read_volatile() }
}

/// Select an item by key. 16-bit, big-endian on the wire.
///
/// # Safety
///
/// Same as `read_data_byte`.
unsafe fn write_selector(value: u16) {
    let addr = (base() + REG_SELECTOR) as *mut u16;
    unsafe { addr.write_volatile(value.to_be()) }
}

/// Write one 32-bit half of the DMA address register. Big-endian on
/// the wire — `.to_be()` on this little-endian target swaps the bytes
/// so the native-endian volatile store places them in the wire order
/// the device expects.
///
/// # Safety
///
/// Same as `read_data_byte`. `offset` must be a valid DMA register
/// offset (`kernel_devices::fwcfg::REG_DMA_ADDR_HIGH`/`_LOW`).
unsafe fn write_dma_reg(offset: usize, value: u32) {
    let addr = (base() + offset) as *mut u32;
    unsafe { addr.write_volatile(value.to_be()) }
}

/// Directory read buffer. Sized generously for QEMU `virt`'s real
/// directory (well under 64 entries); a directory that doesn't fit
/// degrades safely — `kernel_devices::fwcfg::find_file`'s bounds checks
/// just fail to find entries past the truncation point.
const DIR_BUF_LEN: usize = 4096;
static mut DIR_BUF: [u8; DIR_BUF_LEN] = [0u8; DIR_BUF_LEN];

/// Staging for the 16-byte `DmaAccess` descriptor. Written before the
/// register writes that trigger the device's read of it.
static mut DESCRIPTOR: [u8; 16] = [0u8; 16];

/// Staging for the file payload written via DMA. 64 bytes comfortably
/// covers this milestone's only write (`RamfbCfg`, 28 bytes); grow if
/// a larger write is ever needed.
const PAYLOAD_BUF_LEN: usize = 64;
static mut PAYLOAD_BUF: [u8; PAYLOAD_BUF_LEN] = [0u8; PAYLOAD_BUF_LEN];

/// Adapts the kernel's volatile register + `.bss` staging access to
/// the host-tested `FwCfgTransport` trait, mirroring virtio-console's
/// `MmioConsole`.
struct Mmio;

impl FwCfgTransport for Mmio {
    fn write_reg(&mut self, offset: usize, value: u32) {
        // SAFETY: `offset` is always `REG_DMA_ADDR_HIGH`/`_LOW` — the only
        // offsets `kernel_devices::fwcfg::write_file` passes.
        unsafe { write_dma_reg(offset, value) };
    }

    fn write_descriptor(&mut self, bytes: [u8; 16]) {
        // SAFETY: DESCRIPTOR is a `.bss` static touched only from this
        // module, driven exclusively from the boot hart before any other
        // hart or task could race it. The trailing fence orders these
        // writes before the register writes that follow (device-visible
        // trigger) — same reasoning as virtio-console's pre-notify fence.
        unsafe {
            let ptr = (&raw mut DESCRIPTOR).cast::<u8>();
            for (i, b) in bytes.iter().enumerate() {
                ptr.add(i).write_volatile(*b);
            }
        }
        core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
    }

    fn read_descriptor_control(&self) -> u32 {
        // SAFETY: as above.
        unsafe {
            let ptr = (&raw const DESCRIPTOR).cast::<u8>();
            let mut b = [0u8; 4];
            for (i, slot) in b.iter_mut().enumerate() {
                *slot = ptr.add(i).read_volatile();
            }
            u32::from_be_bytes(b)
        }
    }
}

#[derive(Debug)]
pub enum Error {
    /// The device reported a failed transfer (e.g. unknown select key).
    Dma,
}

/// Read the `fw_cfg` file directory and look up `name`.
///
/// # Safety
///
/// `mmu::enable` must have run. Must not run concurrently with another
/// `fwcfg` operation (boot-time, single hart, no interleaving).
pub unsafe fn find_file(name: &str) -> Option<FwCfgFile> {
    unsafe {
        write_selector(SELECTOR_FILE_DIR);
        let ptr = (&raw mut DIR_BUF).cast::<u8>();
        for i in 0..DIR_BUF_LEN {
            ptr.add(i).write_volatile(read_data_byte());
        }
        #[allow(
            clippy::deref_addrof,
            reason = "`&*(&raw const STATIC)` is the required raw-pointer-to-static reference idiom; clippy's deref_addrof misreads `*(&raw const X)` as a redundant `*&`"
        )]
        let buf: &[u8; DIR_BUF_LEN] = &*(&raw const DIR_BUF);
        fwcfg::find_file(buf, name)
    }
}

/// Write `bytes` to the file selected by `select_key` via DMA,
/// blocking until the device confirms completion.
///
/// # Safety
///
/// Same preconditions as `find_file`. `bytes.len()` must not exceed
/// `PAYLOAD_BUF_LEN`.
pub unsafe fn write_file(select_key: u16, bytes: &[u8]) -> Result<(), Error> {
    assert!(bytes.len() <= PAYLOAD_BUF_LEN, "fwcfg payload too large for staging buffer");
    unsafe {
        let payload_ptr = (&raw mut PAYLOAD_BUF).cast::<u8>();
        for (i, b) in bytes.iter().enumerate() {
            payload_ptr.add(i).write_volatile(*b);
        }
        let payload_pa = crate::mmu::va_to_pa(payload_ptr as usize) as u64;
        let desc_pa = crate::mmu::va_to_pa((&raw const DESCRIPTOR).cast::<u8>() as usize) as u64;

        let mut dev = Mmio;
        fwcfg::write_file(&mut dev, select_key, payload_pa, bytes.len() as u32, desc_pa);

        while fwcfg::dma_pending(dev.read_descriptor_control()) {
            core::hint::spin_loop();
        }
        if fwcfg::dma_failed(dev.read_descriptor_control()) {
            return Err(Error::Dma);
        }
    }
    Ok(())
}
