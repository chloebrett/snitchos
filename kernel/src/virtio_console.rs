//! virtio-console (DeviceID 3 over virtio-mmio). Telemetry channel —
//! separate from the NS16550A used for `println!` text.
//!
//! v0.1 scope: discovery only. The handshake, virtqueue setup, and
//! transmit path land in subsequent steps (see plans/virtio-console.md).

use fdt::Fdt;

// --- virtio-mmio register offsets. ---

const REG_MAGIC_VALUE: usize = 0x000;
const REG_VERSION: usize = 0x004;
const REG_DEVICE_ID: usize = 0x008;
const REG_DEVICE_FEATURES: usize = 0x010;
const REG_DEVICE_FEATURES_SEL: usize = 0x014;
const REG_DRIVER_FEATURES: usize = 0x020;
const REG_DRIVER_FEATURES_SEL: usize = 0x024;
const REG_STATUS: usize = 0x070;

// --- Status register bits. The device's state machine. ---

const STATUS_ACKNOWLEDGE: u32 = 0x01;
const STATUS_DRIVER: u32 = 0x02;
const STATUS_DRIVER_OK: u32 = 0x04;
const STATUS_FEATURES_OK: u32 = 0x08;
const STATUS_FAILED: u32 = 0x80;

// --- Constants. ---

/// Magic value at offset 0 of every virtio-mmio slot: the four bytes
/// `"virt"` interpreted as a little-endian u32.
const MAGIC: u32 = 0x74726976;

/// Modern virtio-mmio version. Legacy is 1; we don't support legacy.
const VERSION: u32 = 2;

/// DeviceID for virtio-console.
const DEVICE_ID_CONSOLE: u32 = 3;

/// `VIRTIO_F_VERSION_1` — bit 32 of the feature space. Modern virtio
/// drivers MUST accept this; if the device doesn't advertise it,
/// the device is too old for us.
const F_VERSION_1: u64 = 1 << 32;

/// Read a 32-bit virtio-mmio register.
///
/// # Safety
///
/// `base` must be the MMIO base of a real virtio-mmio device, and
/// `offset` must be a valid register offset within that device's region.
unsafe fn read_reg(base: usize, offset: usize) -> u32 {
  let addr = (base + offset) as *const u32;
  unsafe { addr.read_volatile() }
}

/// Write a 32-bit virtio-mmio register.
///
/// # Safety
///
/// Same as `read_reg`. The caller is also responsible for writing a
/// value that makes sense for the register being written — bad values
/// (e.g. a Status bit pattern that violates the device's state machine)
/// can put the device into the FAILED state.
unsafe fn write_reg(base: usize, offset: usize, value: u32) {
  let addr = (base + offset) as *mut u32;
  unsafe { addr.write_volatile(value) }
}

/// Diagnostic: dump magic/version/device-id for every virtio-mmio slot.
/// Use this to figure out why discovery isn't matching what you expect.
pub fn probe_all_slots(dtb: &Fdt) {
  for node in dtb.all_nodes() {
    let is_virtio = node
      .compatible()
      .map(|c| c.all().any(|s| s == "virtio,mmio"))
      .unwrap_or(false);
    if !is_virtio {
      continue;
    }

    let Some(reg) = node.reg().and_then(|mut r| r.next()) else {
      continue;
    };
    let base = reg.starting_address as usize;

    // SAFETY: the DTB told us this is a virtio-mmio register region.
    let magic = unsafe { read_reg(base, REG_MAGIC_VALUE) };
    let version = unsafe { read_reg(base, REG_VERSION) };
    let device_id = unsafe { read_reg(base, REG_DEVICE_ID) };
    crate::println!(
      "virtio-mmio @ {:#x}: magic={:#x} version={} device_id={}",
      base, magic, version, device_id,
    );
  }
}

/// Walk the DTB for `virtio,mmio` slots, probe each, and return the MMIO
/// base of the first one whose attached device is a virtio-console
/// (DeviceID 3). Returns `None` if no console is attached.
///
/// Known weaknesses:
/// - Returns only the first console found. v0.1 has just one; multi-port
///   handling would need rework.
/// - Doesn't surface *why* a slot was skipped (empty / wrong version /
///   wrong device). For debugging we could log per-slot probe results.
pub fn find_console_base(dtb: &Fdt) -> Option<usize> {
  for node in dtb.all_nodes() {
    let is_virtio = node
      .compatible()
      .map(|c| c.all().any(|s| s == "virtio,mmio"))
      .unwrap_or(false);
    if !is_virtio {
      continue;
    }

    let Some(reg) = node.reg().and_then(|mut r| r.next()) else {
      continue;
    };
    let base = reg.starting_address as usize;

    // SAFETY: the DTB told us this is a virtio-mmio register region.
    let magic = unsafe { read_reg(base, REG_MAGIC_VALUE) };
    if magic != MAGIC {
      continue;
    }

    let version = unsafe { read_reg(base, REG_VERSION) };
    if version != VERSION {
      continue;
    }

    let device_id = unsafe { read_reg(base, REG_DEVICE_ID) };
    if device_id == DEVICE_ID_CONSOLE {
      return Some(base);
    }
  }
  None
}

/// Errors that can arise during the virtio-console handshake.
#[derive(Debug)]
pub enum InitError {
  /// Device doesn't advertise `VIRTIO_F_VERSION_1`. We don't support
  /// pre-1.0 (legacy) virtio at this register layout.
  NoVersion1,
  /// We wrote `FEATURES_OK` but the device cleared it back — meaning it
  /// can't agree to the feature set we offered.
  FeaturesRejected,
}

/// Drive the virtio-mmio handshake on a discovered console device up
/// through `FEATURES_OK`. After this returns `Ok`, the next step is
/// virtqueue setup, then `DRIVER_OK`, then we can transmit.
///
/// # Safety
///
/// `base` must be the MMIO base of a real virtio-mmio device whose
/// DeviceID is `3` (virtio-console). The device must not currently be
/// in use by anyone else — this function resets it.
pub unsafe fn init_handshake(base: usize) -> Result<(), InitError> {
  unsafe {
    // 1. Reset: write 0 to Status, returning the device to a clean state.
    write_reg(base, REG_STATUS, 0);

    // 2. ACKNOWLEDGE: "I see you, device."
    let mut status = STATUS_ACKNOWLEDGE;
    write_reg(base, REG_STATUS, status);

    // 3. DRIVER: "I know how to drive you."
    status |= STATUS_DRIVER;
    write_reg(base, REG_STATUS, status);

    // 4. Feature negotiation. Read both halves of the 64-bit feature
    //    space, decide what we accept, write our subset back.
    write_reg(base, REG_DEVICE_FEATURES_SEL, 0);
    let dev_lo = read_reg(base, REG_DEVICE_FEATURES) as u64;
    write_reg(base, REG_DEVICE_FEATURES_SEL, 1);
    let dev_hi = read_reg(base, REG_DEVICE_FEATURES) as u64;
    let device_features = (dev_hi << 32) | dev_lo;

    if device_features & F_VERSION_1 == 0 {
      write_reg(base, REG_STATUS, status | STATUS_FAILED);
      return Err(InitError::NoVersion1);
    }

    // We accept VERSION_1 only. No CONSOLE_F_SIZE/MULTIPORT/EMERG_WRITE
    // — basic output is enough for v0.1.
    let driver_features = F_VERSION_1;

    write_reg(base, REG_DRIVER_FEATURES_SEL, 0);
    write_reg(base, REG_DRIVER_FEATURES, driver_features as u32);
    write_reg(base, REG_DRIVER_FEATURES_SEL, 1);
    write_reg(base, REG_DRIVER_FEATURES, (driver_features >> 32) as u32);

    // 5. FEATURES_OK: "I've committed; don't change features on me."
    status |= STATUS_FEATURES_OK;
    write_reg(base, REG_STATUS, status);

    // 6. Verify the bit stuck. The device clears FEATURES_OK if it can't
    //    agree to what we offered.
    let read_back = read_reg(base, REG_STATUS);
    if read_back & STATUS_FEATURES_OK == 0 {
      write_reg(base, REG_STATUS, read_back | STATUS_FAILED);
      return Err(InitError::FeaturesRejected);
    }

    // Suppress unused-import warning until step 7 (DRIVER_OK).
    let _ = STATUS_DRIVER_OK;
  }
  Ok(())
}
