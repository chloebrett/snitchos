//! virtio data definitions — the `#[repr(C)]` virtqueue structures and
//! spec constants shared between the kernel driver and host tests. Pure
//! layout: no MMIO, no `unsafe`. The DMA wire contract lives here so
//! the layout-pinning tests run on the host; the kernel side owns the
//! statics, the volatile register access, and the handshake.

// --- virtio-mmio register offsets. ---

pub const REG_MAGIC_VALUE: usize = 0x000;
pub const REG_VERSION: usize = 0x004;
pub const REG_DEVICE_ID: usize = 0x008;
pub const REG_DEVICE_FEATURES: usize = 0x010;
pub const REG_DEVICE_FEATURES_SEL: usize = 0x014;
pub const REG_DRIVER_FEATURES: usize = 0x020;
pub const REG_DRIVER_FEATURES_SEL: usize = 0x024;
pub const REG_QUEUE_SEL: usize = 0x030;
pub const REG_QUEUE_NUM_MAX: usize = 0x034;
pub const REG_QUEUE_NUM: usize = 0x038;
pub const REG_QUEUE_READY: usize = 0x044;
pub const REG_QUEUE_NOTIFY: usize = 0x050;
pub const REG_STATUS: usize = 0x070;
// 64-bit queue address slots — the LOW half lives here, the HIGH half
// at `LOW + 4`. Matches every desc/avail/used address pair in the spec.
pub const REG_QUEUE_DESC_LOW: usize = 0x080;
pub const REG_QUEUE_DRIVER_LOW: usize = 0x090;
pub const REG_QUEUE_DEVICE_LOW: usize = 0x0A0;

// --- Status register bits. The device's state machine. ---

pub const STATUS_ACKNOWLEDGE: u32 = 0x01;
pub const STATUS_DRIVER: u32 = 0x02;
pub const STATUS_DRIVER_OK: u32 = 0x04;
pub const STATUS_FEATURES_OK: u32 = 0x08;
pub const STATUS_FAILED: u32 = 0x80;

/// Magic value at offset 0 of every virtio-mmio slot: the four bytes
/// `"virt"` as a little-endian u32.
pub const MAGIC: u32 = 0x7472_6976;

/// Modern virtio-mmio version. Legacy is 1; we don't support legacy.
pub const VERSION: u32 = 2;

/// `DeviceID` for virtio-console.
pub const DEVICE_ID_CONSOLE: u32 = 3;

/// virtio-console queue indices (no `MULTIPORT` feature).
pub const QUEUE_RX: u32 = 0;
pub const QUEUE_TX: u32 = 1;

/// Read/write access to a virtio-mmio device's 32-bit registers. The
/// kernel implements this over volatile MMIO at the device base; host
/// tests implement it with a `FakeVirtioDevice`. Keeping the handshake
/// logic generic over this trait is what lets it move to kernel-core.
pub trait MmioTransport {
    fn read_reg(&self, offset: usize) -> u32;
    fn write_reg(&mut self, offset: usize, value: u32);
}

/// Number of descriptors in our TX queue. Power of 2 required by spec.
pub const QSIZE: usize = 8;

/// `VIRTIO_F_VERSION_1` — bit 32 of the feature space. Modern virtio
/// drivers MUST accept this; a device that doesn't advertise it is too
/// old (legacy) for our modern register layout.
pub const F_VERSION_1: u64 = 1 << 32;

/// Negotiate features with a device given the 64-bit feature set it
/// advertises. We accept exactly `VIRTIO_F_VERSION_1` and nothing else
/// (no `CONSOLE_F_SIZE` / `MULTIPORT` / `EMERG_WRITE` — basic output
/// suffices). Returns `Some(driver_features)` to write back, or `None`
/// if the device doesn't advertise `VERSION_1` and must be rejected.
pub fn negotiate_features(device_features: u64) -> Option<u64> {
    if device_features & F_VERSION_1 == 0 {
        return None;
    }
    Some(F_VERSION_1)
}

/// Where to place a descriptor in the available ring and what the
/// `avail.idx` counter becomes afterwards. See [`avail_enqueue`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct AvailEnqueue {
    /// Index into `avail.ring[]` to write the descriptor id into.
    /// Wraps at `qsize` (the ring is a power-of-two circular buffer).
    pub ring_slot: usize,
    /// Value to store into `avail.idx` after writing the ring slot.
    /// Grows monotonically and wraps at `u16` — NOT at `qsize`.
    pub next_idx: u16,
}

/// Compute the available-ring placement for the next descriptor given
/// the current `avail.idx`. The device reads `avail.idx` to learn how
/// many entries are live; the driver writes the descriptor id at
/// `idx % qsize` and then advances `idx`. The ring slot wraps at
/// `qsize`; the index itself wraps only at `u16`.
pub fn avail_enqueue(current_idx: u16, qsize: usize) -> AvailEnqueue {
    AvailEnqueue {
        ring_slot: current_idx as usize % qsize,
        next_idx: current_idx.wrapping_add(1),
    }
}

/// Why the virtio feature handshake failed.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum HandshakeError {
    /// Device doesn't advertise `VIRTIO_F_VERSION_1` — too old for us.
    NoVersion1,
    /// We set `FEATURES_OK` but the device cleared it back, meaning it
    /// won't agree to the feature set we committed to.
    FeaturesRejected,
    /// The device's `QueueNumMax` is smaller than the queue size we want.
    QueueTooSmall,
}

/// Drive the virtio-mmio feature handshake over `dev`: reset, announce
/// the driver, negotiate features, commit them, and confirm the device
/// accepted via `FEATURES_OK`. On success the device is in the
/// `FEATURES_OK` state, ready for virtqueue setup and `DRIVER_OK`. On
/// failure the device is moved to `FAILED` and the reason returned.
pub fn feature_handshake<T: MmioTransport>(dev: &mut T) -> Result<(), HandshakeError> {
    // 1. Reset to a clean state.
    dev.write_reg(REG_STATUS, 0);
    // 2. ACKNOWLEDGE: "I see you."
    let mut status = STATUS_ACKNOWLEDGE;
    dev.write_reg(REG_STATUS, status);
    // 3. DRIVER: "I know how to drive you."
    status |= STATUS_DRIVER;
    dev.write_reg(REG_STATUS, status);
    // 4. Read the 64-bit feature space and decide what we accept.
    dev.write_reg(REG_DEVICE_FEATURES_SEL, 0);
    let dev_lo = u64::from(dev.read_reg(REG_DEVICE_FEATURES));
    dev.write_reg(REG_DEVICE_FEATURES_SEL, 1);
    let dev_hi = u64::from(dev.read_reg(REG_DEVICE_FEATURES));
    let device_features = (dev_hi << 32) | dev_lo;
    let Some(driver_features) = negotiate_features(device_features) else {
        dev.write_reg(REG_STATUS, status | STATUS_FAILED);
        return Err(HandshakeError::NoVersion1);
    };
    dev.write_reg(REG_DRIVER_FEATURES_SEL, 0);
    dev.write_reg(REG_DRIVER_FEATURES, driver_features as u32);
    dev.write_reg(REG_DRIVER_FEATURES_SEL, 1);
    dev.write_reg(REG_DRIVER_FEATURES, (driver_features >> 32) as u32);
    // 5. FEATURES_OK: "I've committed."
    status |= STATUS_FEATURES_OK;
    dev.write_reg(REG_STATUS, status);
    // 6. Verify the bit stuck — the device clears it if it disagrees.
    let read_back = dev.read_reg(REG_STATUS);
    if read_back & STATUS_FEATURES_OK == 0 {
        dev.write_reg(REG_STATUS, read_back | STATUS_FAILED);
        return Err(HandshakeError::FeaturesRejected);
    }
    Ok(())
}

/// One queue's selector and the **physical** base addresses of its
/// three ring regions. The caller (kernel) translates VA→PA before
/// building this, since the device has no MMU.
#[derive(Copy, Clone, Debug)]
pub struct QueueConfig {
    pub sel: u32,
    pub desc_pa: u64,
    pub avail_pa: u64,
    pub used_pa: u64,
}

/// Drive the complete virtio-mmio handshake over `dev`: negotiate
/// features, configure every queue in `queues`, then set `DRIVER_OK` to
/// bring the device live. On any queue failure the device is moved to
/// `FAILED` and the error returned — `DRIVER_OK` is never set.
pub fn handshake<T: MmioTransport>(
    dev: &mut T,
    queues: &[QueueConfig],
    qsize: usize,
) -> Result<(), HandshakeError> {
    feature_handshake(dev)?;
    // After feature_handshake the device sits at FEATURES_OK; build the
    // FAILED / DRIVER_OK writes on top of that.
    let status = dev.read_reg(REG_STATUS);
    for q in queues {
        if let Err(e) = setup_queue(dev, q.sel, q.desc_pa, q.avail_pa, q.used_pa, qsize) {
            dev.write_reg(REG_STATUS, status | STATUS_FAILED);
            return Err(e);
        }
    }
    dev.write_reg(REG_STATUS, status | STATUS_DRIVER_OK);
    Ok(())
}

/// Stage `bytes` into `staging` (clamped to its capacity) and hand the
/// staged prefix to `emit`, returning how many bytes were staged. The
/// caller holds whatever lock guards `staging` for the entire call, so
/// the copy and the emit observe the same exclusive buffer — that
/// invariant is what makes the TX path safe under concurrent senders.
/// The kernel's `emit` performs the actual virtqueue transmit; the copy
/// exists because the device needs a kernel-image (`.bss`) address it
/// can translate to a PA, not a heap-VA caller buffer.
pub fn stage_and_emit<F: FnOnce(&[u8])>(staging: &mut [u8], bytes: &[u8], emit: F) -> usize {
    let len = bytes.len().min(staging.len());
    staging[..len].copy_from_slice(&bytes[..len]);
    emit(&staging[..len]);
    len
}

/// Whether the device has advanced the used ring past the snapshot
/// taken before submitting. The driver polls `used.idx`; any change
/// means the device drained our descriptor and the buffer is safe to
/// reuse. `used.idx` wraps at `u16`, so a strict inequality (not `>`)
/// is what correctly treats `MAX -> 0` as progress.
pub fn used_advanced(before: u16, now: u16) -> bool {
    now != before
}

/// Whether our queue of `qsize` descriptors fits within the device's
/// advertised `QueueNumMax`. The device sets the ceiling; we must not
/// configure a queue larger than it allows.
pub fn queue_size_fits(device_max: u32, qsize: usize) -> bool {
    device_max as usize >= qsize
}

/// Write a 64-bit value across a low/high pair of 32-bit registers:
/// the LOW half at `low_offset`, the HIGH half at `low_offset + 4`.
/// Matches every desc/avail/used address slot in the spec.
fn write_reg64<T: MmioTransport>(dev: &mut T, low_offset: usize, value: u64) {
    dev.write_reg(low_offset, value as u32);
    dev.write_reg(low_offset + 4, (value >> 32) as u32);
}

/// Configure one virtqueue against the device: select it, check its
/// `QueueNumMax` against `qsize`, write our queue size, install the
/// three ring base **physical** addresses (descriptor table, available
/// ring, used ring — the caller translates VA→PA since the device has
/// no MMU), and mark it ready.
pub fn setup_queue<T: MmioTransport>(
    dev: &mut T,
    sel: u32,
    desc_pa: u64,
    avail_pa: u64,
    used_pa: u64,
    qsize: usize,
) -> Result<(), HandshakeError> {
    dev.write_reg(REG_QUEUE_SEL, sel);
    let max = dev.read_reg(REG_QUEUE_NUM_MAX);
    if !queue_size_fits(max, qsize) {
        return Err(HandshakeError::QueueTooSmall);
    }
    dev.write_reg(REG_QUEUE_NUM, qsize as u32);
    write_reg64(dev, REG_QUEUE_DESC_LOW, desc_pa);
    write_reg64(dev, REG_QUEUE_DRIVER_LOW, avail_pa);
    write_reg64(dev, REG_QUEUE_DEVICE_LOW, used_pa);
    dev.write_reg(REG_QUEUE_READY, 1);
    Ok(())
}

/// Descriptor flags for the `flags` field of [`VirtqDesc`]. Only
/// `flags = 0` (single buffer, driver-to-device) is used today; the
/// rest are spec constants kept for completeness.
pub const DESC_F_NEXT: u16 = 0x1;
pub const DESC_F_WRITE: u16 = 0x2;
pub const DESC_F_INDIRECT: u16 = 0x4;

/// One descriptor: "buffer at `addr` of `len` bytes, with these flags,
/// optionally chained to the descriptor at index `next`."
#[derive(Copy, Clone, Debug)]
#[repr(C)]
pub struct VirtqDesc {
    pub addr: u64,
    pub len: u32,
    pub flags: u16,
    pub next: u16,
}

/// Available ring: driver tells the device which descriptors to look at.
#[derive(Copy, Clone)]
#[repr(C)]
pub struct VirtqAvail {
    pub flags: u16,
    pub idx: u16,
    pub ring: [u16; QSIZE],
    pub used_event: u16,
}

/// Used ring entry: "descriptor `id` is done; `len` bytes were written
/// (only meaningful for device-to-driver buffers)."
#[derive(Copy, Clone, Debug)]
#[repr(C)]
pub struct VirtqUsedElem {
    pub id: u32,
    pub len: u32,
}

/// Used ring: device tells the driver which descriptors are done.
#[derive(Copy, Clone)]
#[repr(C)]
pub struct VirtqUsed {
    pub flags: u16,
    pub idx: u16,
    pub ring: [VirtqUsedElem; QSIZE],
    pub avail_event: u16,
}

/// All three ring regions for one queue, in one statically-allocated
/// block. The outer 16-byte alignment satisfies the descriptor table's
/// alignment requirement; the inner sub-regions inherit it.
#[repr(C, align(16))]
pub struct Virtqueue {
    pub desc: [VirtqDesc; QSIZE],
    pub avail: VirtqAvail,
    pub used: VirtqUsed,
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;
    use core::mem::{align_of, offset_of, size_of};

    /// A host-side model of a virtio-mmio console device, enough to
    /// drive the handshake against. Serves the discovery/feature/status
    /// registers, models the device's `FEATURES_OK` acceptance rule, and
    /// logs writes so tests can assert on the driver's register sequence.
    struct FakeVirtioDevice {
        status: u32,
        device_features: u64,
        device_features_sel: u32,
        driver_features: u64,
        driver_features_sel: u32,
        queue_num_max: u32,
        reject_features_ok: bool,
        writes: Vec<(usize, u32)>,
    }

    impl FakeVirtioDevice {
        fn new() -> Self {
            Self {
                status: 0,
                device_features: F_VERSION_1,
                device_features_sel: 0,
                driver_features: 0,
                driver_features_sel: 0,
                queue_num_max: 1024,
                reject_features_ok: false,
                writes: Vec::new(),
            }
        }

        /// Override the feature set the device advertises (e.g. a legacy
        /// device with no `VERSION_1`).
        fn with_device_features(mut self, features: u64) -> Self {
            self.device_features = features;
            self
        }

        /// Model a device that clears `FEATURES_OK` even on an otherwise
        /// valid commit — drives the `FeaturesRejected` path.
        fn rejecting_features_ok(mut self) -> Self {
            self.reject_features_ok = true;
            self
        }

        /// Override the device's advertised `QueueNumMax`.
        fn with_queue_num_max(mut self, max: u32) -> Self {
            self.queue_num_max = max;
            self
        }

        /// The register writes the driver has issued, in order.
        fn writes(&self) -> &[(usize, u32)] {
            &self.writes
        }

        /// The device accepts the driver's feature commitment iff it
        /// includes `VERSION_1` and requests nothing the device didn't offer.
        fn features_acceptable(&self) -> bool {
            self.driver_features & F_VERSION_1 != 0
                && self.driver_features & !self.device_features == 0
        }

        fn set_status(&mut self, value: u32) {
            if value == 0 {
                self.status = 0; // reset
                return;
            }
            let reject = self.reject_features_ok || !self.features_acceptable();
            if value & STATUS_FEATURES_OK != 0 && reject {
                self.status = value & !STATUS_FEATURES_OK;
            } else {
                self.status = value;
            }
        }
    }

    impl MmioTransport for FakeVirtioDevice {
        fn read_reg(&self, offset: usize) -> u32 {
            match offset {
                REG_MAGIC_VALUE => MAGIC,
                REG_VERSION => VERSION,
                REG_DEVICE_ID => DEVICE_ID_CONSOLE,
                REG_STATUS => self.status,
                REG_QUEUE_NUM_MAX => self.queue_num_max,
                REG_DEVICE_FEATURES => {
                    if self.device_features_sel == 0 {
                        self.device_features as u32
                    } else {
                        (self.device_features >> 32) as u32
                    }
                }
                _ => 0,
            }
        }

        fn write_reg(&mut self, offset: usize, value: u32) {
            self.writes.push((offset, value));
            match offset {
                REG_STATUS => self.set_status(value),
                REG_DEVICE_FEATURES_SEL => self.device_features_sel = value,
                REG_DRIVER_FEATURES_SEL => self.driver_features_sel = value,
                REG_DRIVER_FEATURES => {
                    if self.driver_features_sel == 0 {
                        self.driver_features =
                            (self.driver_features & !0xFFFF_FFFF) | u64::from(value);
                    } else {
                        self.driver_features =
                            (self.driver_features & 0xFFFF_FFFF) | (u64::from(value) << 32);
                    }
                }
                _ => {}
            }
        }
    }

    fn commit_driver_features(dev: &mut FakeVirtioDevice, features: u64) {
        dev.write_reg(REG_DRIVER_FEATURES_SEL, 0);
        dev.write_reg(REG_DRIVER_FEATURES, features as u32);
        dev.write_reg(REG_DRIVER_FEATURES_SEL, 1);
        dev.write_reg(REG_DRIVER_FEATURES, (features >> 32) as u32);
    }

    #[test]
    fn fake_keeps_features_ok_when_driver_commits_version_1() {
        let mut dev = FakeVirtioDevice::new();
        commit_driver_features(&mut dev, F_VERSION_1);
        dev.write_reg(REG_STATUS, STATUS_FEATURES_OK);
        assert_ne!(dev.read_reg(REG_STATUS) & STATUS_FEATURES_OK, 0);
    }

    #[test]
    fn fake_clears_features_ok_when_driver_omits_version_1() {
        let mut dev = FakeVirtioDevice::new();
        // Driver commits to no features at all → not acceptable.
        dev.write_reg(REG_STATUS, STATUS_FEATURES_OK);
        assert_eq!(dev.read_reg(REG_STATUS) & STATUS_FEATURES_OK, 0);
    }

    fn status_writes(dev: &FakeVirtioDevice) -> Vec<u32> {
        dev.writes()
            .iter()
            .filter(|(off, _)| *off == REG_STATUS)
            .map(|(_, v)| *v)
            .collect()
    }

    #[test]
    fn feature_handshake_drives_status_sequence_in_order() {
        let mut dev = FakeVirtioDevice::new();
        assert!(feature_handshake(&mut dev).is_ok());
        // RESET, ACKNOWLEDGE, +DRIVER, +FEATURES_OK — in this order.
        assert_eq!(
            status_writes(&dev),
            [
                0,
                STATUS_ACKNOWLEDGE,
                STATUS_ACKNOWLEDGE | STATUS_DRIVER,
                STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK,
            ]
        );
        assert_ne!(dev.read_reg(REG_STATUS) & STATUS_FEATURES_OK, 0);
    }

    #[test]
    fn feature_handshake_rejects_device_without_version_1() {
        let mut dev = FakeVirtioDevice::new().with_device_features(0);
        assert_eq!(feature_handshake(&mut dev), Err(HandshakeError::NoVersion1));
        // FAILED was set so the device knows we gave up.
        assert_ne!(dev.read_reg(REG_STATUS) & STATUS_FAILED, 0);
    }

    #[test]
    fn feature_handshake_errors_when_device_clears_features_ok() {
        let mut dev = FakeVirtioDevice::new().rejecting_features_ok();
        assert_eq!(feature_handshake(&mut dev), Err(HandshakeError::FeaturesRejected));
        assert_ne!(dev.read_reg(REG_STATUS) & STATUS_FAILED, 0);
    }

    #[test]
    fn setup_queue_writes_registers_in_order() {
        let mut dev = FakeVirtioDevice::new();
        let (desc, avail, used) = (0x1_2345_6000u64, 0x2_2222_1000u64, 0x3_3333_4000u64);
        assert_eq!(setup_queue(&mut dev, QUEUE_TX, desc, avail, used, QSIZE), Ok(()));
        assert_eq!(
            dev.writes(),
            [
                (REG_QUEUE_SEL, QUEUE_TX),
                (REG_QUEUE_NUM, QSIZE as u32),
                (REG_QUEUE_DESC_LOW, desc as u32),
                (REG_QUEUE_DESC_LOW + 4, (desc >> 32) as u32),
                (REG_QUEUE_DRIVER_LOW, avail as u32),
                (REG_QUEUE_DRIVER_LOW + 4, (avail >> 32) as u32),
                (REG_QUEUE_DEVICE_LOW, used as u32),
                (REG_QUEUE_DEVICE_LOW + 4, (used >> 32) as u32),
                (REG_QUEUE_READY, 1),
            ]
        );
    }

    #[test]
    fn setup_queue_rejects_when_device_max_below_our_size() {
        let mut dev = FakeVirtioDevice::new().with_queue_num_max(QSIZE as u32 - 1);
        assert_eq!(
            setup_queue(&mut dev, QUEUE_TX, 0, 0, 0, QSIZE),
            Err(HandshakeError::QueueTooSmall)
        );
        // Bailed after selecting the queue: no NUM / addresses / READY.
        assert_eq!(dev.writes(), [(REG_QUEUE_SEL, QUEUE_TX)]);
    }

    fn two_queues() -> [QueueConfig; 2] {
        [
            QueueConfig { sel: QUEUE_RX, desc_pa: 0x1000, avail_pa: 0x2000, used_pa: 0x3000 },
            QueueConfig { sel: QUEUE_TX, desc_pa: 0x4000, avail_pa: 0x5000, used_pa: 0x6000 },
        ]
    }

    #[test]
    fn handshake_finishes_with_driver_ok_after_queues_are_ready() {
        let mut dev = FakeVirtioDevice::new();
        assert_eq!(handshake(&mut dev, &two_queues(), QSIZE), Ok(()));
        // DRIVER_OK is the final status write.
        let acked = STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK;
        assert_eq!(*status_writes(&dev).last().unwrap(), acked | STATUS_DRIVER_OK);
        assert_ne!(dev.read_reg(REG_STATUS) & STATUS_DRIVER_OK, 0);
        // ...and it lands after the last QUEUE_READY, not before.
        let writes = dev.writes();
        let last_ready = writes.iter().rposition(|(o, _)| *o == REG_QUEUE_READY).unwrap();
        let driver_ok = writes
            .iter()
            .rposition(|(o, v)| *o == REG_STATUS && v & STATUS_DRIVER_OK != 0)
            .unwrap();
        assert!(last_ready < driver_ok);
    }

    #[test]
    fn handshake_fails_without_driver_ok_when_a_queue_is_too_small() {
        let mut dev = FakeVirtioDevice::new().with_queue_num_max(QSIZE as u32 - 1);
        assert_eq!(handshake(&mut dev, &two_queues(), QSIZE), Err(HandshakeError::QueueTooSmall));
        assert_ne!(dev.read_reg(REG_STATUS) & STATUS_FAILED, 0);
        assert_eq!(dev.read_reg(REG_STATUS) & STATUS_DRIVER_OK, 0);
    }

    #[test]
    fn handshake_propagates_no_version_1_without_touching_queues() {
        let mut dev = FakeVirtioDevice::new().with_device_features(0);
        assert_eq!(handshake(&mut dev, &two_queues(), QSIZE), Err(HandshakeError::NoVersion1));
        assert_eq!(dev.read_reg(REG_STATUS) & STATUS_DRIVER_OK, 0);
        // Never reached queue setup.
        assert!(dev.writes().iter().all(|(o, _)| *o != REG_QUEUE_READY));
    }

    #[test]
    fn virtq_desc_matches_spec_layout() {
        // virtio spec: a descriptor is exactly 16 bytes, fields in this
        // order. Reordering or retyping a field silently breaks DMA.
        assert_eq!(size_of::<VirtqDesc>(), 16);
        assert_eq!(offset_of!(VirtqDesc, addr), 0);
        assert_eq!(offset_of!(VirtqDesc, len), 8);
        assert_eq!(offset_of!(VirtqDesc, flags), 12);
        assert_eq!(offset_of!(VirtqDesc, next), 14);
    }

    #[test]
    fn virtq_used_elem_matches_spec_layout() {
        // spec: a used-ring element is 8 bytes — u32 id, u32 len.
        assert_eq!(size_of::<VirtqUsedElem>(), 8);
        assert_eq!(offset_of!(VirtqUsedElem, id), 0);
        assert_eq!(offset_of!(VirtqUsedElem, len), 4);
    }

    #[test]
    fn negotiate_accepts_version_1_and_offers_only_it() {
        // Device advertises VERSION_1 plus some other features; we
        // commit to VERSION_1 only.
        let device = F_VERSION_1 | 0xFF;
        assert_eq!(negotiate_features(device), Some(F_VERSION_1));
    }

    #[test]
    fn negotiate_rejects_when_version_1_absent() {
        // Lower 32 bits all set, but bit 32 (VERSION_1) clear → reject.
        assert_eq!(negotiate_features(0xFFFF_FFFF), None);
    }

    #[test]
    fn negotiate_rejects_empty_feature_set() {
        assert_eq!(negotiate_features(0), None);
    }

    #[test]
    fn queue_size_fits_when_device_max_exceeds_ours() {
        assert!(queue_size_fits(1024, QSIZE));
    }

    #[test]
    fn queue_size_fits_at_exact_match() {
        // Device max equal to our size is a fit (>=, not >).
        assert!(queue_size_fits(QSIZE as u32, QSIZE));
    }

    #[test]
    fn queue_size_does_not_fit_when_device_max_is_one_short() {
        assert!(!queue_size_fits(QSIZE as u32 - 1, QSIZE));
    }

    #[test]
    fn queue_size_does_not_fit_when_device_max_is_zero() {
        assert!(!queue_size_fits(0, QSIZE));
    }

    #[test]
    fn avail_enqueue_at_start() {
        let e = avail_enqueue(0, QSIZE);
        assert_eq!(e.ring_slot, 0);
        assert_eq!(e.next_idx, 1);
    }

    #[test]
    fn avail_enqueue_at_last_slot_before_wrap() {
        // idx 7 with qsize 8: last ring slot; idx climbs to 8.
        let e = avail_enqueue(7, QSIZE);
        assert_eq!(e.ring_slot, 7);
        assert_eq!(e.next_idx, 8);
    }

    #[test]
    fn avail_enqueue_ring_slot_wraps_while_idx_keeps_climbing() {
        // idx 8 with qsize 8: the RING slot wraps to 0, but avail.idx
        // does not — it keeps growing monotonically.
        let e = avail_enqueue(8, QSIZE);
        assert_eq!(e.ring_slot, 0);
        assert_eq!(e.next_idx, 9);
    }

    #[test]
    fn avail_enqueue_idx_wraps_at_u16_max() {
        // The avail.idx counter wraps at u16; the ring slot is its
        // value mod qsize.
        let e = avail_enqueue(u16::MAX, QSIZE);
        assert_eq!(e.ring_slot, (u16::MAX as usize) % QSIZE);
        assert_eq!(e.next_idx, 0);
    }

    #[test]
    fn stage_and_emit_copies_then_emits_the_staged_bytes() {
        let mut staging = [0u8; 8];
        let mut seen = Vec::new();
        let n = stage_and_emit(&mut staging, &[1, 2, 3], |s| seen.extend_from_slice(s));
        assert_eq!(n, 3);
        assert_eq!(&staging[..3], &[1, 2, 3]);
        assert_eq!(seen, [1, 2, 3]);
    }

    #[test]
    fn stage_and_emit_clamps_to_staging_capacity() {
        let mut staging = [0u8; 4];
        let mut seen = Vec::new();
        let n = stage_and_emit(&mut staging, &[1, 2, 3, 4, 5, 6], |s| seen.extend_from_slice(s));
        assert_eq!(n, 4);
        assert_eq!(seen, [1, 2, 3, 4]); // emit saw only what fit
    }

    #[test]
    fn used_not_advanced_while_idx_unchanged() {
        // Device hasn't touched used.idx yet — keep spinning.
        assert!(!used_advanced(5, 5));
    }

    #[test]
    fn used_advanced_when_idx_moves_forward() {
        assert!(used_advanced(5, 6));
    }

    #[test]
    fn used_advanced_across_u16_wrap() {
        // used.idx wraps at u16; MAX -> 0 still counts as "advanced".
        assert!(used_advanced(u16::MAX, 0));
    }

    #[test]
    fn virtqueue_is_16_byte_aligned() {
        // The descriptor table requires 16-byte alignment; the outer
        // align(16) on Virtqueue is what guarantees it for the whole
        // statically-allocated block.
        assert_eq!(align_of::<Virtqueue>(), 16);
    }
}
