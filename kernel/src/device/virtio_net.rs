//! virtio-net (`DeviceID` 1 over virtio-mmio). The QEMU/snemu NIC behind the
//! `net=` UDP telemetry transport (M2.5). The UDP/IP/Ethernet datagram is built
//! upstream (`kernel-net`); this driver only puts a complete Ethernet frame on
//! the TX queue, prefixed with the 12-byte virtio-net header.
//!
//! Structurally a near-clone of [`super::virtio_console`]: discover via the DTB,
//! drive the handshake, transmit one descriptor at a time and spin on the used
//! ring. The differences are exactly two — it probes for `DeviceID` 1, and it
//! stages `[virtio_net_hdr ‖ frame]` (via [`stage_net_tx`]) rather than raw
//! bytes.
//!
//! DEDUP: `read_reg`/`write_reg`/`queue_config`/`transmit` mirror
//! `virtio_console`. They can't be shared as-is because each device owns its own
//! `static mut` queues (so `transmit` is tied to a specific `TX_QUEUE`); a
//! follow-up should lift the queue into a field and extract a shared
//! `virtio_mmio` device. Kept additive here to avoid editing the console file
//! while the surrounding tree is churning.

use fdt::Fdt;

use kernel_devices::virtio::{
    MAGIC, QSIZE, QUEUE_RX, QUEUE_TX, REG_DEVICE_ID, REG_MAGIC_VALUE, REG_QUEUE_NOTIFY,
    REG_VERSION, VERSION, VirtqAvail, VirtqDesc, VirtqUsed, VirtqUsedElem, Virtqueue,
};
use kernel_devices::virtio_net::{DEVICE_ID_NET, NET_HDR_LEN, stage_net_tx};

/// Static TX queue for the virtio-net device. Lives in `.bss`; one instance,
/// one TX path.
static mut TX_QUEUE: Virtqueue = Virtqueue {
    desc: [VirtqDesc { addr: 0, len: 0, flags: 0, next: 0 }; QSIZE],
    avail: VirtqAvail { flags: 0, idx: 0, ring: [0; QSIZE], used_event: 0 },
    used: VirtqUsed { flags: 0, idx: 0, ring: [VirtqUsedElem { id: 0, len: 0 }; QSIZE], avail_event: 0 },
};

/// Static RX queue. We never receive (telemetry is egress-only), but virtio-net
/// wants its receiveq configured or it may refuse to process TX — same as the
/// console's unused RX queue.
static mut RX_QUEUE: Virtqueue = Virtqueue {
    desc: [VirtqDesc { addr: 0, len: 0, flags: 0, next: 0 }; QSIZE],
    avail: VirtqAvail { flags: 0, idx: 0, ring: [0; QSIZE], used_event: 0 },
    used: VirtqUsed { flags: 0, idx: 0, ring: [VirtqUsedElem { id: 0, len: 0 }; QSIZE], avail_event: 0 },
};

/// Read a 32-bit virtio-mmio register. DEDUP: mirrors `virtio_console::read_reg`.
///
/// # Safety
/// `base` must be a real virtio-mmio base and `offset` a valid register offset.
unsafe fn read_reg(base: usize, offset: usize) -> u32 {
    let addr = (base + offset) as *const u32;
    unsafe { addr.read_volatile() }
}

/// Write a 32-bit virtio-mmio register. DEDUP: mirrors `virtio_console::write_reg`.
///
/// # Safety
/// As `read_reg`; the caller must write a value valid for the register.
unsafe fn write_reg(base: usize, offset: usize, value: u32) {
    let addr = (base + offset) as *mut u32;
    unsafe { addr.write_volatile(value) }
}

/// Build a `QueueConfig`, translating the three ring regions' VAs to physical
/// addresses (the device has no MMU). DEDUP: mirrors `virtio_console::queue_config`.
///
/// # Safety
/// `queue` must outlive the device's use of it (in practice `'static`).
unsafe fn queue_config(sel: u32, queue: *const Virtqueue) -> kernel_devices::virtio::QueueConfig {
    // SAFETY: `queue` is a 'static Virtqueue; taking its field addresses and
    // translating them to PAs is sound.
    unsafe {
        kernel_devices::virtio::QueueConfig {
            sel,
            desc_pa: crate::mmu::va_to_pa(&raw const (*queue).desc as usize) as u64,
            avail_pa: crate::mmu::va_to_pa(&raw const (*queue).avail as usize) as u64,
            used_pa: crate::mmu::va_to_pa(&raw const (*queue).used as usize) as u64,
        }
    }
}

/// Walk the DTB for `virtio,mmio` slots, probe each, and return the higher-half
/// VA of the first one whose device is a virtio-net (`DeviceID` 1). `None` if no
/// net device is attached (the common case: production/board builds don't have
/// one). The single genuinely-new function versus the console driver.
fn find_net_base(dtb: &Fdt) -> Option<usize> {
    for node in dtb.all_nodes() {
        let is_virtio = node
            .compatible()
            .is_some_and(|c| c.all().any(|s| s == "virtio,mmio"));
        if !is_virtio {
            continue;
        }
        let Some(reg) = node.reg().and_then(|mut r| r.next()) else {
            continue;
        };
        let base = reg.starting_address as usize + crate::mmu::KERNEL_OFFSET;

        // SAFETY: the DTB told us this is a virtio-mmio register region.
        if unsafe { read_reg(base, REG_MAGIC_VALUE) } != MAGIC {
            continue;
        }
        if unsafe { read_reg(base, REG_VERSION) } != VERSION {
            continue;
        }
        if unsafe { read_reg(base, REG_DEVICE_ID) } == DEVICE_ID_NET {
            return Some(base);
        }
    }
    None
}

/// TX staging: the 12-byte virtio-net header plus a full Ethernet frame
/// (14 + 1500 MTU). Frames arrive already datagram-framed from `kernel-net`.
const TX_STAGING_LEN: usize = NET_HDR_LEN + 14 + 1500;

/// Mutex-guarded per-device state: the MMIO base and the TX staging buffer. The
/// buffer lives inside the mutex so the descriptor's PA translation is correct
/// (heap-VA caller frames can't be translated) and so the stage→transmit section
/// is exclusive across concurrent senders. Same discipline as the console.
pub struct TxStaging {
    base: usize,
    buf: [u8; TX_STAGING_LEN],
}

/// The global virtio-net handle, set once at boot via [`init`].
pub static NET: crate::sync::Once<crate::sync::Mutex<TxStaging>> = crate::sync::Once::new();

/// virtio-net initialization failures: DTB discovery (`NotFound`) or the pure
/// handshake (`Handshake`).
#[derive(Debug)]
pub enum InitError {
    /// No virtio-mmio slot advertised `DeviceID` 1 (net).
    NotFound,
    /// The device bring-up handshake failed — see the wrapped reason.
    Handshake(
        #[expect(dead_code, reason = "surfaced via Debug in the init-failure log, not matched on")]
        kernel_devices::virtio::HandshakeError,
    ),
}

impl From<kernel_devices::virtio::HandshakeError> for InitError {
    fn from(e: kernel_devices::virtio::HandshakeError) -> Self {
        InitError::Handshake(e)
    }
}

/// A virtio-mmio device addressed by its higher-half base, adapting the kernel's
/// volatile register access to the host-testable `MmioTransport`.
struct MmioNet {
    base: usize,
}

impl kernel_devices::virtio::MmioTransport for MmioNet {
    fn read_reg(&self, offset: usize) -> u32 {
        // SAFETY: `base` is a discovered virtio-mmio base and `offset` a register.
        unsafe { read_reg(self.base, offset) }
    }
    fn write_reg(&mut self, offset: usize, value: u32) {
        // SAFETY: as above; the handshake only writes valid register values.
        unsafe { write_reg(self.base, offset, value) }
    }
}

/// Discover the virtio-net device in the DTB, drive the handshake, and store its
/// higher-half MMIO base in [`NET`]. After `Ok`, [`send`] is usable.
///
/// # Safety
/// The DTB must be valid and `mmu::enable` must have run (higher-half MMIO live).
pub unsafe fn init(dtb: &Fdt) -> Result<(), InitError> {
    let base = find_net_base(dtb).ok_or(InitError::NotFound)?;
    unsafe { init_handshake(base)? };
    NET.call_once(|| crate::sync::Mutex::new(TxStaging { base, buf: [0u8; TX_STAGING_LEN] }));
    Ok(())
}

/// Transmit one complete Ethernet `frame` out the virtio-net TX queue, blocking
/// until the device drains it. No-ops if [`init`] hasn't run. The 12-byte
/// virtio-net header is prepended here; `frame` is the datagram from `kernel-net`.
pub fn send(frame: &[u8]) {
    let Some(handle) = NET.get() else {
        return;
    };
    let mut staging = handle.lock();
    let base = staging.base;
    // SAFETY: the guard is held for the whole `stage_net_tx` call, so this hart
    // is the sole writer to the staging buffer and sole driver of the virtqueue
    // while `transmit` runs. `staged` points into the `.bss` buffer, so
    // `transmit`'s `va_to_pa` is correct.
    stage_net_tx(&mut staging.buf, frame, |staged| unsafe {
        transmit(base, staged);
    });
}

/// Drive the virtio-mmio handshake on the discovered net device through
/// `DRIVER_OK`. DEDUP: mirrors `virtio_console::init_handshake`.
///
/// # Safety
/// `base` must be a real virtio-mmio device with `DeviceID` 1, not in use
/// elsewhere — this resets it.
unsafe fn init_handshake(base: usize) -> Result<(), InitError> {
    let mut dev = MmioNet { base };
    // Configure both receiveq (0) and transmitq (1); the device may drop TX if
    // its RX queue is unconfigured, even though we never receive.
    // SAFETY: RX_QUEUE / TX_QUEUE are 'static.
    let queues = unsafe {
        [
            queue_config(QUEUE_RX, &raw const RX_QUEUE),
            queue_config(QUEUE_TX, &raw const TX_QUEUE),
        ]
    };
    kernel_devices::virtio::handshake(&mut dev, &queues, QSIZE).map_err(InitError::from)
}

/// The canonical virtio TX cycle: fill descriptor 0, push it into the available
/// ring, notify, and spin on the used ring. DEDUP: mirrors
/// `virtio_console::transmit` (differs only in the `TX_QUEUE` static it drives).
///
/// # Safety
/// `base` must be a net device past `init_handshake`; `bytes` must stay valid for
/// the call (the trailing spin makes stack/`.bss` buffers safe).
unsafe fn transmit(base: usize, bytes: &[u8]) {
    // SAFETY: TX_QUEUE is a static mut guarded by the caller's mutex.
    let desc_ptr = unsafe { &raw mut TX_QUEUE.desc[0] };
    unsafe {
        desc_ptr.write_volatile(VirtqDesc {
            addr: crate::mmu::va_to_pa(bytes.as_ptr() as usize) as u64,
            len: bytes.len() as u32,
            flags: 0,
            next: 0,
        });
    }
    unsafe {
        let avail_idx_before = (&raw const TX_QUEUE.avail.idx).read_volatile();
        let used_idx_before = (&raw const TX_QUEUE.used.idx).read_volatile();
        let enq = kernel_devices::virtio::avail_enqueue(avail_idx_before, QSIZE);
        (&raw mut TX_QUEUE.avail.ring[enq.ring_slot]).write_volatile(0);
        (&raw mut TX_QUEUE.avail.idx).write_volatile(enq.next_idx);
        core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
        write_reg(base, REG_QUEUE_NOTIFY, QUEUE_TX);
        while !kernel_devices::virtio::used_advanced(
            used_idx_before,
            (&raw const TX_QUEUE.used.idx).read_volatile(),
        ) {}
    }
}
