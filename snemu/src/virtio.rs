//! A minimal virtio-mmio virtio-console device: enough of the register file and
//! status state machine for the kernel's discovery walk and feature/queue
//! handshake to complete. The descriptor-ring transmit path is layer 2.
//!
//! snemu deliberately reimplements the device half of the virtio contract
//! rather than sharing `kernel_core::virtio`, so it stays an independent oracle.
//! The constants below mirror the virtio-mmio spec (and that module).

/// virtio-mmio register offsets (32-bit registers).
const REG_MAGIC_VALUE: usize = 0x000;
const REG_VERSION: usize = 0x004;
const REG_DEVICE_ID: usize = 0x008;
const REG_DEVICE_FEATURES: usize = 0x010;
const REG_DEVICE_FEATURES_SEL: usize = 0x014;
const REG_DRIVER_FEATURES: usize = 0x020;
const REG_DRIVER_FEATURES_SEL: usize = 0x024;
const REG_QUEUE_NUM_MAX: usize = 0x034;
const REG_STATUS: usize = 0x070;

/// Status state-machine bits the driver writes / reads back.
const STATUS_FEATURES_OK: u32 = 0x08;

/// `"virt"` little-endian — the magic at offset 0 of every virtio-mmio slot.
const MAGIC: u32 = 0x7472_6976;
/// Modern (non-legacy) virtio-mmio version.
const VERSION: u32 = 2;
/// `DeviceID` for virtio-console.
const DEVICE_ID_CONSOLE: u32 = 3;
/// `VIRTIO_F_VERSION_1` — bit 32, the only feature we advertise.
const F_VERSION_1: u64 = 1 << 32;
/// The descriptor count we advertise as the per-queue ceiling. The kernel
/// requires this to be `>=` its own `QSIZE` (8).
const QUEUE_NUM_MAX: u32 = 8;

/// The virtio-mmio window on the QEMU `virt` machine: 8 slots, `0x1000` apart.
pub(crate) const MMIO_BASE: u64 = 0x1000_1000;
const MMIO_STRIDE: u64 = 0x1000;
const MMIO_COUNT: u64 = 8;
pub(crate) const MMIO_END: u64 = MMIO_BASE + MMIO_STRIDE * MMIO_COUNT;

/// QEMU places the virtio-console at the highest slot (`0x1000_8000`); the
/// other seven slots are present but empty (`DeviceID == 0`).
const CONSOLE_SLOT: u64 = (0x1000_8000 - MMIO_BASE) / MMIO_STRIDE;

/// The virtio-mmio device block: one live console plus seven empty slots.
pub(crate) struct Virtio {
    status: u32,
    device_features_sel: u32,
    driver_features_sel: u32,
    driver_features: u64,
}

impl Virtio {
    pub(crate) fn new() -> Self {
        Self {
            status: 0,
            device_features_sel: 0,
            driver_features_sel: 0,
            driver_features: 0,
        }
    }

    /// Whether `addr` falls in the virtio-mmio register window.
    pub(crate) fn in_window(addr: u64) -> bool {
        (MMIO_BASE..MMIO_END).contains(&addr)
    }

    /// Read a 32-bit register at a guest physical `addr` in the window.
    pub(crate) fn read(&self, addr: u64) -> u32 {
        let (slot, offset) = decode(addr);
        // Empty slots are present but advertise no device, so the probe skips
        // them; only the console slot serves the full register file.
        if slot != CONSOLE_SLOT {
            return match offset {
                REG_MAGIC_VALUE => MAGIC,
                REG_VERSION => VERSION,
                _ => 0, // including REG_DEVICE_ID -> 0 (no device here)
            };
        }
        match offset {
            REG_MAGIC_VALUE => MAGIC,
            REG_VERSION => VERSION,
            REG_DEVICE_ID => DEVICE_ID_CONSOLE,
            REG_QUEUE_NUM_MAX => QUEUE_NUM_MAX,
            REG_STATUS => self.status,
            REG_DEVICE_FEATURES if self.device_features_sel == 0 => F_VERSION_1 as u32,
            REG_DEVICE_FEATURES => (F_VERSION_1 >> 32) as u32,
            _ => 0,
        }
    }

    /// Write a 32-bit register at a guest physical `addr` in the window. Only
    /// the console slot has writable state; writes to empty slots are dropped.
    pub(crate) fn write(&mut self, addr: u64, value: u32) {
        let (slot, offset) = decode(addr);
        if slot != CONSOLE_SLOT {
            return;
        }
        match offset {
            REG_STATUS => self.set_status(value),
            REG_DEVICE_FEATURES_SEL => self.device_features_sel = value,
            REG_DRIVER_FEATURES_SEL => self.driver_features_sel = value,
            REG_DRIVER_FEATURES => self.set_driver_features(value),
            _ => {} // queue config + notify: layer 2
        }
    }

    /// Accumulate a 32-bit driver-feature half into the 64-bit set, selected by
    /// the most recent `DRIVER_FEATURES_SEL`.
    fn set_driver_features(&mut self, value: u32) {
        if self.driver_features_sel == 0 {
            self.driver_features = (self.driver_features & !0xFFFF_FFFF) | u64::from(value);
        } else {
            self.driver_features = (self.driver_features & 0xFFFF_FFFF) | (u64::from(value) << 32);
        }
    }

    /// Apply the status state machine: a zero write resets; committing
    /// `FEATURES_OK` only sticks if the driver's features are acceptable
    /// (includes `VERSION_1` and requests nothing we didn't offer), else the
    /// device clears the bit so the driver knows it was rejected.
    fn set_status(&mut self, value: u32) {
        if value == 0 {
            self.status = 0;
            return;
        }
        let acceptable =
            self.driver_features & F_VERSION_1 != 0 && self.driver_features & !F_VERSION_1 == 0;
        if value & STATUS_FEATURES_OK != 0 && !acceptable {
            self.status = value & !STATUS_FEATURES_OK;
        } else {
            self.status = value;
        }
    }
}

/// Split a window address into its slot index and register offset.
fn decode(addr: u64) -> (u64, usize) {
    let slot = (addr - MMIO_BASE) / MMIO_STRIDE;
    let offset = ((addr - MMIO_BASE) % MMIO_STRIDE) as usize;
    (slot, offset)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONSOLE_BASE: u64 = 0x1000_8000;
    const EMPTY_BASE: u64 = 0x1000_1000;

    #[test]
    fn console_slot_answers_the_discovery_registers() {
        let dev = Virtio::new();
        assert_eq!(dev.read(CONSOLE_BASE + REG_MAGIC_VALUE as u64), MAGIC);
        assert_eq!(dev.read(CONSOLE_BASE + REG_VERSION as u64), VERSION);
        assert_eq!(dev.read(CONSOLE_BASE + REG_DEVICE_ID as u64), DEVICE_ID_CONSOLE);
    }

    #[test]
    fn empty_slot_reports_no_device() {
        let dev = Virtio::new();
        // Present (magic + version) but DeviceID 0, so the probe skips it.
        assert_eq!(dev.read(EMPTY_BASE + REG_MAGIC_VALUE as u64), MAGIC);
        assert_eq!(dev.read(EMPTY_BASE + REG_VERSION as u64), VERSION);
        assert_eq!(dev.read(EMPTY_BASE + REG_DEVICE_ID as u64), 0);
    }

    #[test]
    fn device_features_expose_version_1_in_the_high_half() {
        let mut dev = Virtio::new();
        dev.write(CONSOLE_BASE + REG_DEVICE_FEATURES_SEL as u64, 0);
        assert_eq!(dev.read(CONSOLE_BASE + REG_DEVICE_FEATURES as u64), 0);
        dev.write(CONSOLE_BASE + REG_DEVICE_FEATURES_SEL as u64, 1);
        assert_eq!(dev.read(CONSOLE_BASE + REG_DEVICE_FEATURES as u64), 1); // bit 32
    }

    #[test]
    fn queue_num_max_meets_the_kernel_minimum() {
        let dev = Virtio::new();
        assert!(dev.read(CONSOLE_BASE + REG_QUEUE_NUM_MAX as u64) >= 8);
    }

    /// Drive the feature commitment the kernel performs: select+write the
    /// driver feature halves, then set `FEATURES_OK`.
    fn commit_and_set_features_ok(dev: &mut Virtio, features: u64) {
        dev.write(CONSOLE_BASE + REG_DRIVER_FEATURES_SEL as u64, 0);
        dev.write(CONSOLE_BASE + REG_DRIVER_FEATURES as u64, features as u32);
        dev.write(CONSOLE_BASE + REG_DRIVER_FEATURES_SEL as u64, 1);
        dev.write(CONSOLE_BASE + REG_DRIVER_FEATURES as u64, (features >> 32) as u32);
        dev.write(CONSOLE_BASE + REG_STATUS as u64, STATUS_FEATURES_OK);
    }

    #[test]
    fn features_ok_sticks_when_driver_commits_version_1() {
        let mut dev = Virtio::new();
        commit_and_set_features_ok(&mut dev, F_VERSION_1);
        assert_ne!(dev.read(CONSOLE_BASE + REG_STATUS as u64) & STATUS_FEATURES_OK, 0);
    }

    #[test]
    fn features_ok_is_cleared_when_driver_omits_version_1() {
        let mut dev = Virtio::new();
        commit_and_set_features_ok(&mut dev, 0);
        assert_eq!(dev.read(CONSOLE_BASE + REG_STATUS as u64) & STATUS_FEATURES_OK, 0);
    }

    #[test]
    fn status_resets_to_zero_on_a_zero_write() {
        let mut dev = Virtio::new();
        dev.write(CONSOLE_BASE + REG_STATUS as u64, 0x01);
        dev.write(CONSOLE_BASE + REG_STATUS as u64, 0);
        assert_eq!(dev.read(CONSOLE_BASE + REG_STATUS as u64), 0);
    }
}
