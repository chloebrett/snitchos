//! A minimal virtio-mmio virtio-console device: discovery + handshake (layer 1)
//! and the TX descriptor-ring transmit path (layer 2). On a TX queue notify the
//! device walks the driver-configured virtqueue in guest RAM, pulls the
//! transmitted bytes, and publishes the used ring so the driver's poll completes.
//!
//! snemu deliberately reimplements the device half of the virtio contract
//! rather than sharing `kernel_core::virtio`, so it stays an independent oracle.
//! The constants below mirror the virtio-mmio spec (and that module).

use crate::mem::Memory;

/// virtio-mmio register offsets (32-bit registers).
const REG_MAGIC_VALUE: usize = 0x000;
const REG_VERSION: usize = 0x004;
const REG_DEVICE_ID: usize = 0x008;
const REG_DEVICE_FEATURES: usize = 0x010;
const REG_DEVICE_FEATURES_SEL: usize = 0x014;
const REG_DRIVER_FEATURES: usize = 0x020;
const REG_DRIVER_FEATURES_SEL: usize = 0x024;
const REG_QUEUE_SEL: usize = 0x030;
const REG_QUEUE_NUM_MAX: usize = 0x034;
const REG_QUEUE_NUM: usize = 0x038;
const REG_QUEUE_READY: usize = 0x044;
const REG_QUEUE_NOTIFY: usize = 0x050;
const REG_STATUS: usize = 0x070;
// 64-bit queue-address slots: LOW half here, HIGH half at LOW + 4.
const REG_QUEUE_DESC_LOW: usize = 0x080;
const REG_QUEUE_DESC_HIGH: usize = REG_QUEUE_DESC_LOW + 4;
const REG_QUEUE_DRIVER_LOW: usize = 0x090; // available ring
const REG_QUEUE_DRIVER_HIGH: usize = REG_QUEUE_DRIVER_LOW + 4;
const REG_QUEUE_DEVICE_LOW: usize = 0x0A0; // used ring
const REG_QUEUE_DEVICE_HIGH: usize = REG_QUEUE_DEVICE_LOW + 4;

/// Status state-machine bits the driver writes / reads back.
const STATUS_FEATURES_OK: u32 = 0x08;

/// `"virt"` little-endian — the magic at offset 0 of every virtio-mmio slot.
const MAGIC: u32 = 0x7472_6976;
/// Modern (non-legacy) virtio-mmio version.
const VERSION: u32 = 2;
/// `DeviceID` for virtio-console.
const DEVICE_ID_CONSOLE: u32 = 3;
/// `DeviceID` for virtio-net.
const DEVICE_ID_NET: u32 = 1;
/// Bytes stripped from each net TX datagram to reach the telemetry payload:
/// the virtio-net header (12) + Ethernet (14) + IPv4 (20) + UDP (8) headers.
const NET_HEADER_STRIP: usize = 12 + 14 + 20 + 8;
/// `VIRTIO_F_VERSION_1` — bit 32, the only feature we advertise.
const F_VERSION_1: u64 = 1 << 32;
/// The descriptor count we advertise as the per-queue ceiling. The kernel
/// requires this to be `>=` its own `QSIZE` (8).
const QUEUE_NUM_MAX: u32 = 8;

/// virtio-console queue indices (no `MULTIPORT`): the driver transmits on TX.
const TX_QUEUE: usize = 1;

/// Field offsets within the `#[repr(C)]` virtqueue structures (mirrors
/// `kernel_core::virtio`, whose layout tests pin these).
const DESC_SIZE: u64 = 16;
const DESC_ADDR: u64 = 0;
const DESC_LEN: u64 = 8;
const DESC_FLAGS: u64 = 12;
const DESC_NEXT: u64 = 14;
const DESC_F_NEXT: u16 = 0x1;
const AVAIL_IDX: u64 = 2;
const AVAIL_RING: u64 = 4;
const USED_IDX: u64 = 2;
const USED_RING: u64 = 4;
const USED_ELEM_SIZE: u64 = 8;

/// The virtio-mmio window on the QEMU `virt` machine: 8 slots, `0x1000` apart.
pub(crate) const MMIO_BASE: u64 = 0x1000_1000;
const MMIO_STRIDE: u64 = 0x1000;
const MMIO_COUNT: u64 = 8;
pub(crate) const MMIO_END: u64 = MMIO_BASE + MMIO_STRIDE * MMIO_COUNT;

/// QEMU places the virtio-console at the highest slot (`0x1000_8000`); the
/// other slots are present but empty (`DeviceID == 0`) unless a device is
/// modelled there.
const CONSOLE_SLOT: u64 = (0x1000_8000 - MMIO_BASE) / MMIO_STRIDE;
/// The slot the virtio-net device answers on (`0x1000_7000`), just below the
/// console. Its DTB node advertises it so `virtio_console`-style discovery finds
/// it by probing `DeviceID`.
const NET_SLOT: u64 = (0x1000_7000 - MMIO_BASE) / MMIO_STRIDE;

/// One virtqueue's driver-installed configuration plus the device's progress
/// cursor through the available ring (`processed`, which also indexes the used
/// ring since each buffer yields exactly one used entry).
#[derive(Clone, Copy, Default)]
struct Queue {
    num: u32,
    desc: u64,
    avail: u64,
    used: u64,
    ready: bool,
    processed: u16,
}

/// What a live slot's TX bytes mean, which is the only thing that differs
/// between the two devices we model.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DeviceKind {
    /// Raw telemetry bytes on the wire (the QEMU virtio-console path).
    Console,
    /// UDP datagrams — the payload begins after [`NET_HEADER_STRIP`] header bytes.
    Net,
}

/// One live virtio-mmio device: its register/queue state plus the bytes it has
/// pulled off its TX queue. `Console` and `Net` share all of this and differ
/// only in `DeviceID` and how [`Device::absorb`] interprets a drained buffer.
#[derive(Clone)]
struct Device {
    kind: DeviceKind,
    device_id: u32,
    status: u32,
    device_features_sel: u32,
    driver_features_sel: u32,
    driver_features: u64,
    queue_sel: u32,
    queues: [Queue; 2],
    output: Vec<u8>,
}

impl Device {
    fn new(kind: DeviceKind, device_id: u32) -> Self {
        Self {
            kind,
            device_id,
            status: 0,
            device_features_sel: 0,
            driver_features_sel: 0,
            driver_features: 0,
            queue_sel: 0,
            queues: [Queue::default(); 2],
            output: Vec::new(),
        }
    }

    /// Read a 32-bit register by `offset` within this device's slot.
    fn read(&self, offset: usize) -> u32 {
        match offset {
            REG_MAGIC_VALUE => MAGIC,
            REG_VERSION => VERSION,
            REG_DEVICE_ID => self.device_id,
            REG_QUEUE_NUM_MAX => QUEUE_NUM_MAX,
            REG_STATUS => self.status,
            REG_DEVICE_FEATURES if self.device_features_sel == 0 => F_VERSION_1 as u32,
            REG_DEVICE_FEATURES => (F_VERSION_1 >> 32) as u32,
            _ => 0,
        }
    }

    /// Write a 32-bit register by `offset` within this device's slot.
    fn write(&mut self, offset: usize, value: u32) {
        match offset {
            REG_STATUS => self.set_status(value),
            REG_DEVICE_FEATURES_SEL => self.device_features_sel = value,
            REG_DRIVER_FEATURES_SEL => self.driver_features_sel = value,
            REG_DRIVER_FEATURES => self.set_driver_features(value),
            REG_QUEUE_SEL => self.queue_sel = value,
            _ => self.write_queue_reg(offset, value), // queue config + notify
        }
    }

    /// Apply a write to one of the selected queue's configuration registers.
    fn write_queue_reg(&mut self, offset: usize, value: u32) {
        let Some(q) = self.queues.get_mut(self.queue_sel as usize) else {
            return;
        };
        match offset {
            REG_QUEUE_NUM => q.num = value,
            REG_QUEUE_READY => q.ready = value != 0,
            REG_QUEUE_DESC_LOW => put_low(&mut q.desc, value),
            REG_QUEUE_DESC_HIGH => put_high(&mut q.desc, value),
            REG_QUEUE_DRIVER_LOW => put_low(&mut q.avail, value),
            REG_QUEUE_DRIVER_HIGH => put_high(&mut q.avail, value),
            REG_QUEUE_DEVICE_LOW => put_low(&mut q.used, value),
            REG_QUEUE_DEVICE_HIGH => put_high(&mut q.used, value),
            _ => {} // REG_QUEUE_NOTIFY (serviced by the bus) and anything else
        }
    }

    /// Service the TX queue: absorb every newly-available descriptor chain and
    /// publish a used-ring entry for each, so the driver's `used.idx` poll
    /// completes. The device has no MMU; the ring addresses the driver installed
    /// are guest physical addresses read straight from RAM.
    fn service_tx(&mut self, ram: &mut Memory) {
        let mut q = self.queues[TX_QUEUE]; // Copy: frees `self` for output pushes
        if !q.ready || q.num == 0 {
            return;
        }
        let qsize = q.num as u16;
        let avail_idx = ram.read_u16(q.avail + AVAIL_IDX).unwrap_or(q.processed);
        while q.processed != avail_idx {
            let ring_slot = u64::from(q.processed % qsize);
            let head = ram.read_u16(q.avail + AVAIL_RING + ring_slot * 2).unwrap_or(0);
            let buf = read_chain(ram, q.desc, head, qsize);
            let len = buf.len() as u32;
            self.absorb(&buf);
            // Publish the used-ring entry for this buffer, then advance used.idx.
            let used_slot = u64::from(q.processed % qsize);
            let elem = q.used + USED_RING + used_slot * USED_ELEM_SIZE;
            let _ = ram.write_u32(elem, u32::from(head));
            let _ = ram.write_u32(elem + 4, len);
            q.processed = q.processed.wrapping_add(1);
            let _ = ram.write_u16(q.used + USED_IDX, q.processed);
        }
        self.queues[TX_QUEUE] = q;
    }

    /// Interpret one drained TX buffer per the device kind: console bytes are the
    /// telemetry stream as-is; a net datagram's telemetry payload begins after
    /// the fixed Ethernet/IP/UDP + virtio-net header prefix.
    fn absorb(&mut self, buf: &[u8]) {
        match self.kind {
            DeviceKind::Console => self.output.extend_from_slice(buf),
            DeviceKind::Net => {
                if let Some(payload) = buf.get(NET_HEADER_STRIP..) {
                    self.output.extend_from_slice(payload);
                }
            }
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

/// Walk a descriptor chain from `head`, collecting each buffer's bytes, and
/// return them. Bounded by `qsize` so a malformed cyclic chain can't spin
/// forever. The device has no MMU — `desc_base` and the buffer addresses are
/// guest physical.
fn read_chain(ram: &Memory, desc_base: u64, head: u16, qsize: u16) -> Vec<u8> {
    let mut id = head;
    let mut buf = Vec::new();
    for _ in 0..qsize {
        let d = desc_base + u64::from(id) * DESC_SIZE;
        let addr = ram.read_u64(d + DESC_ADDR).unwrap_or(0);
        let len = ram.read_u32(d + DESC_LEN).unwrap_or(0);
        let flags = ram.read_u16(d + DESC_FLAGS).unwrap_or(0);
        let next = ram.read_u16(d + DESC_NEXT).unwrap_or(0);
        for i in 0..u64::from(len) {
            buf.push(ram.read_u8(addr + i).unwrap_or(0));
        }
        if flags & DESC_F_NEXT == 0 {
            break;
        }
        id = next;
    }
    buf
}

/// The virtio-mmio device block: a live console (slot 7) and a live net device
/// (slot 6), with the remaining slots present but empty (`DeviceID == 0`).
#[derive(Clone)]
pub(crate) struct Virtio {
    console: Device,
    net: Device,
}

impl Virtio {
    pub(crate) fn new() -> Self {
        Self {
            console: Device::new(DeviceKind::Console, DEVICE_ID_CONSOLE),
            net: Device::new(DeviceKind::Net, DEVICE_ID_NET),
        }
    }

    /// The live device answering on `slot`, if any (empty slots return `None`).
    fn device_at(&self, slot: u64) -> Option<&Device> {
        match slot {
            CONSOLE_SLOT => Some(&self.console),
            NET_SLOT => Some(&self.net),
            _ => None,
        }
    }

    fn device_at_mut(&mut self, slot: u64) -> Option<&mut Device> {
        match slot {
            CONSOLE_SLOT => Some(&mut self.console),
            NET_SLOT => Some(&mut self.net),
            _ => None,
        }
    }

    /// Whether `addr` falls in the virtio-mmio register window.
    pub(crate) fn in_window(addr: u64) -> bool {
        (MMIO_BASE..MMIO_END).contains(&addr)
    }

    /// Whether a window write to `addr` is a queue-notify (the bus services the
    /// TX queues against guest RAM after such a write).
    pub(crate) fn is_notify(addr: u64) -> bool {
        decode(addr).1 == REG_QUEUE_NOTIFY
    }

    /// Raw telemetry bytes the console has pulled off its TX queue.
    pub(crate) fn tx_output(&self) -> &[u8] {
        &self.console.output
    }

    /// Telemetry payload bytes the net device has extracted from its TX
    /// datagrams (headers stripped) — the same COBS stream the console emits.
    pub(crate) fn net_tx_output(&self) -> &[u8] {
        &self.net.output
    }

    /// Read a 32-bit register at a guest physical `addr` in the window. A live
    /// slot serves its full register file; an empty slot is present (magic +
    /// version) but advertises no device, so the probe skips it.
    pub(crate) fn read(&self, addr: u64) -> u32 {
        let (slot, offset) = decode(addr);
        match self.device_at(slot) {
            Some(dev) => dev.read(offset),
            None => match offset {
                REG_MAGIC_VALUE => MAGIC,
                REG_VERSION => VERSION,
                _ => 0, // including REG_DEVICE_ID -> 0 (no device here)
            },
        }
    }

    /// Write a 32-bit register at a guest physical `addr` in the window. Only a
    /// live slot has writable state; writes to empty slots are dropped.
    pub(crate) fn write(&mut self, addr: u64, value: u32) {
        let (slot, offset) = decode(addr);
        if let Some(dev) = self.device_at_mut(slot) {
            dev.write(offset, value);
        }
    }

    /// Service both devices' TX queues. A notify names one queue, but servicing
    /// the other is a no-op (it has no new available entries), so both run.
    pub(crate) fn service_tx(&mut self, ram: &mut Memory) {
        self.console.service_tx(ram);
        self.net.service_tx(ram);
    }
}

/// Set the low / high 32-bit half of a 64-bit register slot.
fn put_low(slot: &mut u64, value: u32) {
    *slot = (*slot & !0xFFFF_FFFF) | u64::from(value);
}
fn put_high(slot: &mut u64, value: u32) {
    *slot = (*slot & 0xFFFF_FFFF) | (u64::from(value) << 32);
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
    use crate::mem::{Memory, RAM_BASE};

    const CONSOLE_BASE: u64 = 0x1000_8000;
    const NET_BASE: u64 = 0x1000_7000;
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

    /// Configure the device's TX queue to point at the three ring regions.
    fn configure_tx_queue(dev: &mut Virtio, desc: u64, avail: u64, used: u64, num: u32) {
        let w = |dev: &mut Virtio, off: usize, v: u32| dev.write(CONSOLE_BASE + off as u64, v);
        w(dev, REG_QUEUE_SEL, TX_QUEUE as u32);
        w(dev, REG_QUEUE_NUM, num);
        w(dev, REG_QUEUE_DESC_LOW, desc as u32);
        w(dev, REG_QUEUE_DESC_HIGH, (desc >> 32) as u32);
        w(dev, REG_QUEUE_DRIVER_LOW, avail as u32);
        w(dev, REG_QUEUE_DRIVER_HIGH, (avail >> 32) as u32);
        w(dev, REG_QUEUE_DEVICE_LOW, used as u32);
        w(dev, REG_QUEUE_DEVICE_HIGH, (used >> 32) as u32);
        w(dev, REG_QUEUE_READY, 1);
    }

    #[test]
    fn tx_notify_drains_a_descriptor_and_publishes_the_used_ring() {
        let desc_pa = RAM_BASE + 0x1000;
        let avail_pa = RAM_BASE + 0x2000;
        let used_pa = RAM_BASE + 0x3000;
        let buf_pa = RAM_BASE + 0x4000;
        let payload = b"hi!";
        let mut mem = Memory::new(0x10000);
        // Payload bytes the descriptor points at.
        for (i, &b) in payload.iter().enumerate() {
            mem.write_u8(buf_pa + i as u64, b).unwrap();
        }
        // Descriptor 0: single buffer, driver-to-device (flags 0).
        mem.write_u64(desc_pa + DESC_ADDR, buf_pa).unwrap();
        mem.write_u32(desc_pa + DESC_LEN, payload.len() as u32).unwrap();
        mem.write_u16(desc_pa + DESC_FLAGS, 0).unwrap();
        mem.write_u16(desc_pa + DESC_NEXT, 0).unwrap();
        // Available ring: one entry (idx 1), pointing at descriptor 0.
        mem.write_u16(avail_pa + AVAIL_IDX, 1).unwrap();
        mem.write_u16(avail_pa + AVAIL_RING, 0).unwrap();

        let mut dev = Virtio::new();
        configure_tx_queue(&mut dev, desc_pa, avail_pa, used_pa, 8);
        dev.write(CONSOLE_BASE + REG_QUEUE_NOTIFY as u64, TX_QUEUE as u32);
        dev.service_tx(&mut mem);

        assert_eq!(dev.tx_output(), payload); // bytes pulled off the queue
        // Used ring published: idx advanced, entry { id: 0, len: 3 }.
        assert_eq!(mem.read_u16(used_pa + USED_IDX).unwrap(), 1);
        assert_eq!(mem.read_u32(used_pa + USED_RING).unwrap(), 0);
        assert_eq!(mem.read_u32(used_pa + USED_RING + 4).unwrap(), payload.len() as u32);
    }

    #[test]
    fn tx_notify_follows_a_chained_descriptor() {
        let desc_pa = RAM_BASE + 0x1000;
        let avail_pa = RAM_BASE + 0x2000;
        let used_pa = RAM_BASE + 0x3000;
        let buf0 = RAM_BASE + 0x4000;
        let buf1 = RAM_BASE + 0x5000;
        let mut mem = Memory::new(0x10000);
        mem.write_u8(buf0, b'a').unwrap();
        mem.write_u8(buf1, b'b').unwrap();
        mem.write_u8(buf1 + 1, b'c').unwrap();
        // Descriptor 0 -> descriptor 1 via DESC_F_NEXT.
        mem.write_u64(desc_pa + DESC_ADDR, buf0).unwrap();
        mem.write_u32(desc_pa + DESC_LEN, 1).unwrap();
        mem.write_u16(desc_pa + DESC_FLAGS, DESC_F_NEXT).unwrap();
        mem.write_u16(desc_pa + DESC_NEXT, 1).unwrap();
        mem.write_u64(desc_pa + DESC_SIZE + DESC_ADDR, buf1).unwrap();
        mem.write_u32(desc_pa + DESC_SIZE + DESC_LEN, 2).unwrap();
        mem.write_u16(desc_pa + DESC_SIZE + DESC_FLAGS, 0).unwrap();
        mem.write_u16(avail_pa + AVAIL_IDX, 1).unwrap();
        mem.write_u16(avail_pa + AVAIL_RING, 0).unwrap();

        let mut dev = Virtio::new();
        configure_tx_queue(&mut dev, desc_pa, avail_pa, used_pa, 8);
        dev.service_tx(&mut mem);

        assert_eq!(dev.tx_output(), b"abc"); // both buffers, in chain order
        assert_eq!(mem.read_u32(used_pa + USED_RING + 4).unwrap(), 3); // total len
    }

    #[test]
    fn net_slot_answers_discovery_with_the_net_device_id() {
        let dev = Virtio::new();
        assert_eq!(dev.read(NET_BASE + REG_MAGIC_VALUE as u64), MAGIC);
        assert_eq!(dev.read(NET_BASE + REG_DEVICE_ID as u64), DEVICE_ID_NET);
    }

    #[test]
    fn net_tx_strips_the_header_to_the_payload() {
        let desc_pa = RAM_BASE + 0x1000;
        let avail_pa = RAM_BASE + 0x2000;
        let used_pa = RAM_BASE + 0x3000;
        let buf_pa = RAM_BASE + 0x4000;
        let payload = b"COBS-frame-bytes";
        let mut mem = Memory::new(0x10000);
        // A full datagram in one buffer: NET_HEADER_STRIP header bytes (arbitrary
        // 0xEE — the model strips by length, not by parsing) then the payload.
        for i in 0..NET_HEADER_STRIP {
            mem.write_u8(buf_pa + i as u64, 0xEE).unwrap();
        }
        for (i, &b) in payload.iter().enumerate() {
            mem.write_u8(buf_pa + (NET_HEADER_STRIP + i) as u64, b).unwrap();
        }
        let total = (NET_HEADER_STRIP + payload.len()) as u32;
        mem.write_u64(desc_pa + DESC_ADDR, buf_pa).unwrap();
        mem.write_u32(desc_pa + DESC_LEN, total).unwrap();
        mem.write_u16(desc_pa + DESC_FLAGS, 0).unwrap();
        mem.write_u16(desc_pa + DESC_NEXT, 0).unwrap();
        mem.write_u16(avail_pa + AVAIL_IDX, 1).unwrap();
        mem.write_u16(avail_pa + AVAIL_RING, 0).unwrap();

        let mut dev = Virtio::new();
        // Configure the NET slot's TX queue.
        let w = |dev: &mut Virtio, off: usize, v: u32| dev.write(NET_BASE + off as u64, v);
        w(&mut dev, REG_QUEUE_SEL, TX_QUEUE as u32);
        w(&mut dev, REG_QUEUE_NUM, 8);
        w(&mut dev, REG_QUEUE_DESC_LOW, desc_pa as u32);
        w(&mut dev, REG_QUEUE_DESC_HIGH, (desc_pa >> 32) as u32);
        w(&mut dev, REG_QUEUE_DRIVER_LOW, avail_pa as u32);
        w(&mut dev, REG_QUEUE_DRIVER_HIGH, (avail_pa >> 32) as u32);
        w(&mut dev, REG_QUEUE_DEVICE_LOW, used_pa as u32);
        w(&mut dev, REG_QUEUE_DEVICE_HIGH, (used_pa >> 32) as u32);
        w(&mut dev, REG_QUEUE_READY, 1);
        dev.service_tx(&mut mem);

        // Only the payload survives — the 54 header bytes are stripped, and the
        // full datagram length (not the stripped length) is published as used.
        assert_eq!(dev.net_tx_output(), payload, "headers stripped, payload kept");
        assert!(dev.tx_output().is_empty(), "the console device saw nothing");
        assert_eq!(mem.read_u32(used_pa + USED_RING + 4).unwrap(), total);
    }

    #[test]
    fn unready_queue_is_not_serviced() {
        let mut dev = Virtio::new();
        let mut mem = Memory::new(0x10000);
        mem.write_u16(RAM_BASE + 0x2000 + AVAIL_IDX, 1).unwrap();
        // Configure the addresses but never mark the queue ready.
        let w = |dev: &mut Virtio, off: usize, v: u32| dev.write(CONSOLE_BASE + off as u64, v);
        w(&mut dev, REG_QUEUE_SEL, TX_QUEUE as u32);
        w(&mut dev, REG_QUEUE_NUM, 8);
        w(&mut dev, REG_QUEUE_DRIVER_LOW, (RAM_BASE + 0x2000) as u32);
        w(&mut dev, REG_QUEUE_DRIVER_HIGH, ((RAM_BASE + 0x2000) >> 32) as u32);
        dev.service_tx(&mut mem);
        assert!(dev.tx_output().is_empty());
    }
}
