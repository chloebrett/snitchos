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
const REG_QUEUE_SEL: usize = 0x030;
const REG_QUEUE_NUM_MAX: usize = 0x034;
const REG_QUEUE_NUM: usize = 0x038;
const REG_QUEUE_READY: usize = 0x044;
const REG_QUEUE_NOTIFY: usize = 0x050;
const REG_STATUS: usize = 0x070;
// 64-bit queue address slots — written via `write_reg64`, which fills
// the LOW half here and the HIGH half at `LOW + 4`.
const REG_QUEUE_DESC_LOW: usize = 0x080;
const REG_QUEUE_DRIVER_LOW: usize = 0x090;
const REG_QUEUE_DEVICE_LOW: usize = 0x0A0;

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

/// Descriptor flags. Used in the `flags` field of `VirtqDesc`.
#[expect(dead_code, reason = "spec constants; only flags=0 is used today")]
const DESC_F_NEXT: u16 = 0x1;
#[expect(dead_code, reason = "spec constants; only flags=0 is used today")]
const DESC_F_WRITE: u16 = 0x2;
#[expect(dead_code, reason = "spec constants; only flags=0 is used today")]
const DESC_F_INDIRECT: u16 = 0x4;

/// virtio-console queue indices (no MULTIPORT feature).
const QUEUE_RX: u32 = 0;
const QUEUE_TX: u32 = 1;

/// Number of descriptors in our TX queue. Power of 2 required by spec.
const QSIZE: usize = 8;

/// One descriptor: "buffer at `addr` of `len` bytes, with these flags,
/// optionally chained to the descriptor at index `next`."
#[derive(Copy, Clone, Debug)]
#[repr(C)]
struct VirtqDesc {
    addr: u64,
    len: u32,
    flags: u16,
    next: u16,
}

/// Available ring: driver tells the device which descriptors to look at.
#[derive(Copy, Clone)]
#[repr(C)]
struct VirtqAvail {
    flags: u16,
    idx: u16,
    ring: [u16; QSIZE],
    used_event: u16,
}

/// Used ring entry: "descriptor `id` is done; `len` bytes were written
/// (only meaningful for device-to-driver buffers)."
#[derive(Copy, Clone, Debug)]
#[repr(C)]
struct VirtqUsedElem {
    id: u32,
    len: u32,
}

/// Used ring: device tells the driver which descriptors are done.
#[derive(Copy, Clone)]
#[repr(C)]
struct VirtqUsed {
    flags: u16,
    idx: u16,
    ring: [VirtqUsedElem; QSIZE],
    avail_event: u16,
}

/// All three ring regions for one queue, in one statically-allocated
/// block. The outer 16-byte alignment satisfies the descriptor table's
/// alignment requirement; the inner sub-regions inherit it.
#[repr(C, align(16))]
struct Virtqueue {
    desc: [VirtqDesc; QSIZE],
    avail: VirtqAvail,
    used: VirtqUsed,
}

/// Static TX queue for the virtio-console. Lives in `.bss`. Single
/// instance — we have one console and one TX path in v0.1.
static mut TX_QUEUE: Virtqueue = Virtqueue {
    desc: [VirtqDesc {
        addr: 0,
        len: 0,
        flags: 0,
        next: 0,
    }; QSIZE],
    avail: VirtqAvail {
        flags: 0,
        idx: 0,
        ring: [0; QSIZE],
        used_event: 0,
    },
    used: VirtqUsed {
        flags: 0,
        idx: 0,
        ring: [VirtqUsedElem { id: 0, len: 0 }; QSIZE],
        avail_event: 0,
    },
};

/// Static RX queue. The virtio-console spec requires the driver to
/// initialize both port 0 receive and transmit queues. We don't actually
/// receive in v0.1, so this stays empty — but the device needs to see it
/// configured or it may refuse to process TX.
static mut RX_QUEUE: Virtqueue = Virtqueue {
    desc: [VirtqDesc {
        addr: 0,
        len: 0,
        flags: 0,
        next: 0,
    }; QSIZE],
    avail: VirtqAvail {
        flags: 0,
        idx: 0,
        ring: [0; QSIZE],
        used_event: 0,
    },
    used: VirtqUsed {
        flags: 0,
        idx: 0,
        ring: [VirtqUsedElem { id: 0, len: 0 }; QSIZE],
        avail_event: 0,
    },
};

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

/// Write a 64-bit value to a pair of virtio-mmio low/high 32-bit
/// registers. Convention: `low_offset` is the LOW half; `low_offset+4`
/// is the HIGH half — matches every desc/avail/used address pair in
/// the spec.
///
/// # Safety
///
/// Same as `write_reg`. Both registers must belong to the same logical
/// 64-bit address slot.
unsafe fn write_reg64(base: usize, low_offset: usize, value: u64) {
    unsafe {
        write_reg(base, low_offset, value as u32);
        write_reg(base, low_offset + 4, (value >> 32) as u32);
    }
}

/// Configure one virtqueue against the device: select it, check the
/// device's max queue size against ours, write our queue size, install
/// the three ring base addresses (descriptor table, available ring,
/// used ring), and mark it ready.
///
/// # Safety
///
/// `base` must be the MMIO base of a virtio-mmio device that's in the
/// FEATURES_OK state. `queue` must outlive the device's use of it (in
/// practice that's `'static`, which is why we take a pointer not a
/// reference — the device writes to the used ring, and we don't want
/// to imply Rust's aliasing rules cover device accesses).
unsafe fn setup_queue(base: usize, sel: u32, queue: *const Virtqueue) -> Result<(), InitError> {
    unsafe {
        write_reg(base, REG_QUEUE_SEL, sel);
        let max = read_reg(base, REG_QUEUE_NUM_MAX);
        if (max as usize) < QSIZE {
            return Err(InitError::QueueTooSmall);
        }
        write_reg(base, REG_QUEUE_NUM, QSIZE as u32);

        // Device has no MMU — addresses we give it must be physical.
        // `va_to_pa` is a no-op when the kernel runs at identity PC
        // (the cast already yields physical); it strips KERNEL_OFFSET
        // once the kernel runs at higher-half PC.
        let desc = crate::mmu::va_to_pa(&raw const (*queue).desc as usize) as u64;
        let avail = crate::mmu::va_to_pa(&raw const (*queue).avail as usize) as u64;
        let used = crate::mmu::va_to_pa(&raw const (*queue).used as usize) as u64;
        write_reg64(base, REG_QUEUE_DESC_LOW, desc);
        write_reg64(base, REG_QUEUE_DRIVER_LOW, avail);
        write_reg64(base, REG_QUEUE_DEVICE_LOW, used);
        write_reg(base, REG_QUEUE_READY, 1);
    }
    Ok(())
}

/// Diagnostic: dump magic/version/device-id for every virtio-mmio slot.
/// Use this to figure out why discovery isn't matching what you expect.
/// Kept around as a tool, not wired into the boot path.
#[expect(dead_code, reason = "diagnostic; kept as a debugging tool, not wired into boot")]
fn probe_all_slots(dtb: &Fdt) {
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
            base,
            magic,
            version,
            device_id,
        );
    }
}

/// Walk the DTB for `virtio,mmio` slots, probe each, and return the
/// MMIO base of the first one whose attached device is a
/// virtio-console (DeviceID 3). Returns the **higher-half VA** of the
/// base; probes run through that VA too, so the function doesn't
/// depend on identity-MMIO being live. Returns `None` if no console
/// is attached.
///
/// Known weaknesses:
/// - Returns only the first console found. v0.1 has just one;
///   multi-port handling would need rework.
/// - Doesn't surface *why* a slot was skipped (empty / wrong version /
///   wrong device). For debugging we could log per-slot probe results.
fn find_console_base(dtb: &Fdt) -> Option<usize> {
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
        let base = reg.starting_address as usize + crate::mmu::KERNEL_OFFSET;

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

/// The global virtio-console handle. Holds the device's MMIO base
/// address. Set once at boot via `init`.
///
/// `spin::Mutex<usize>` is overkill today (base is immutable after
/// init), but the lock will serialize concurrent senders once we have
/// interrupts or SMP. Same pattern as the NS16550 `UART`.
pub static CONSOLE: spin::Once<spin::Mutex<usize>> = spin::Once::new();

/// Errors that can arise during virtio-console initialization.
#[derive(Debug)]
pub enum InitError {
    /// No virtio-mmio slot advertised DeviceID 3 (console).
    NotFound,
    /// Device doesn't advertise `VIRTIO_F_VERSION_1`. We don't support
    /// pre-1.0 (legacy) virtio at this register layout.
    NoVersion1,
    /// We wrote `FEATURES_OK` but the device cleared it back — meaning it
    /// can't agree to the feature set we offered.
    FeaturesRejected,
    /// Device's `QueueNumMax` is smaller than the queue size we want.
    /// Shouldn't happen — QEMU advertises max 1024.
    QueueTooSmall,
}

/// Discover the virtio-console in the DTB, drive the handshake, and
/// store the device's MMIO base in the `CONSOLE` static as a
/// higher-half VA. After this returns `Ok`, `send` is usable from
/// anywhere.
///
/// # Safety
///
/// The DTB must be valid, and `mmu::enable` must have run (so the
/// higher-half MMIO mapping is live) — the same precondition the rest
/// of post-MMU boot relies on.
pub unsafe fn init(dtb: &Fdt) -> Result<(), InitError> {
    // `base` is a higher-half VA — `find_console_base` translates
    // through `KERNEL_OFFSET` so all MMIO in this function runs
    // through the higher-half mapping, not identity.
    let base = find_console_base(dtb).ok_or(InitError::NotFound)?;
    unsafe { init_handshake(base)? };
    CONSOLE.call_once(|| spin::Mutex::new(base));
    Ok(())
}

/// Send a buffer of bytes out the kernel's virtio-console. Silently
/// no-ops if `init` hasn't completed (matches the println macro's
/// "pre-init bytes are lost" behavior).
pub fn send(bytes: &[u8]) {
    let Some(handle) = CONSOLE.get() else {
        return;
    };
    let base = *handle.lock();
    // SAFETY: CONSOLE was populated by `init`, which ran handshake +
    // queue setup against `base`. The device is ready to receive on
    // the TX queue.
    unsafe { transmit(base, bytes) };
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
unsafe fn init_handshake(base: usize) -> Result<(), InitError> {
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

        // 7. Virtqueue setup. virtio-console requires BOTH port 0 RX
        //    (queue 0) and TX (queue 1) to be configured, even if we
        //    never plan to receive — the device may silently drop our
        //    TX otherwise.
        if let Err(e) = setup_queue(base, QUEUE_RX, &raw const RX_QUEUE) {
            write_reg(base, REG_STATUS, status | STATUS_FAILED);
            return Err(e);
        }
        if let Err(e) = setup_queue(base, QUEUE_TX, &raw const TX_QUEUE) {
            write_reg(base, REG_STATUS, status | STATUS_FAILED);
            return Err(e);
        }

        // 8. DRIVER_OK: "I'm fully set up; treat me as a working driver."
        status |= STATUS_DRIVER_OK;
        write_reg(base, REG_STATUS, status);
    }
    Ok(())
}

/// Send a buffer of bytes out the virtio-console TX queue, blocking
/// until the device confirms it has drained the buffer.
///
/// The flow is the canonical virtio TX cycle:
///
/// 1. Fill descriptor slot 0 with `{addr, len, flags=0, next=0}`. We
///    always use slot 0 because we only allow one TX in flight at a
///    time (we spin on completion before returning), so slot 0 is
///    always free by the time we re-enter.
/// 2. Snapshot the avail/used index counters *before* submitting, so
///    we know what value to bump and what to wait on.
/// 3. Push descriptor index `0` into the available ring at
///    `avail.ring[avail.idx % QSIZE]`, then bump `avail.idx`. The
///    device watches `avail.idx` and processes anything new.
/// 4. Write `QUEUE_TX` to `QueueNotify`. This is the only trap we
///    take on the TX path — everything else is silent shared-memory
///    coordination. Without the notify, the device never knows we
///    submitted.
/// 5. Poll `used.idx` until it advances. The advance means the
///    device has read our buffer; `bytes` is safe to release.
///
/// # Safety
///
/// - `base` must be the MMIO base of a virtio-console that has
///   completed `init_handshake` (specifically: TX queue set up and
///   `DRIVER_OK` set).
/// - `bytes` must remain valid for the entire call. We hand the
///   device a raw pointer; if `bytes` is freed or moves, the device
///   reads garbage. The spin-wait at the end is what makes
///   stack-allocated buffers safe — by the time we return, the
///   device is done.
///
/// # Known weaknesses
///
/// - **Single descriptor in flight.** We reuse slot 0 forever; no
///   concurrency on the TX path. A real driver would use a free-list
///   of slots and submit multiple before any completion.
/// - **Polling, not interrupt-driven.** The CPU spins until
///   `used.idx` moves. Wastes cycles. v0.3 (interrupts milestone)
///   would replace this with a wait queue + IRQ handler.
/// - **No memory barriers between the descriptor write and the
///   notify.** On QEMU, all writes are visible by the time we take
///   the trap, so this works in practice. On real hardware, the CPU
///   could reorder the notify ahead of the descriptor fill — we'd
///   need a write fence (`fence ow,ow` on RISC-V) before the notify.
/// - **No timeout.** If the device wedges, we spin forever. A real
///   driver bounds the wait and surfaces a `DeviceStuck` error.
/// - **No queue-init check.** SAFETY comment says you must call
///   `init_handshake` first; if you don't, this writes into an
///   un-set-up TX_QUEUE that the device isn't reading. The device
///   never advances `used.idx`, and we spin forever.
unsafe fn transmit(base: usize, bytes: &[u8]) {
    // 1. Fill descriptor slot 0 with the buffer pointer + length.
    // SAFETY: TX_QUEUE is a static mut; we hold the device's mutex via
    // the caller (`send`), so no other writer touches it concurrently.
    let desc_ptr = unsafe { &raw mut TX_QUEUE.desc[0] };
    unsafe {
        desc_ptr.write_volatile(VirtqDesc {
            // Device has no MMU — translate VA to PA.
            addr: crate::mmu::va_to_pa(bytes.as_ptr() as usize) as u64,
            len: bytes.len() as u32,
            flags: 0, // single buffer, driver-to-device, no chaining
            next: 0,  // irrelevant without NEXT
        });
    }

    unsafe {
        // 2. Snapshot the ring index counters.
        let avail_idx_before = (&raw const TX_QUEUE.avail.idx).read_volatile();
        let used_idx_before = (&raw const TX_QUEUE.used.idx).read_volatile();

        // 3. Push descriptor index 0 into the available ring, then
        //    bump avail.idx. Index field wraps naturally as u16.
        (&raw mut TX_QUEUE.avail.ring[(avail_idx_before as usize) % QSIZE]).write_volatile(0);
        (&raw mut TX_QUEUE.avail.idx).write_volatile(avail_idx_before.wrapping_add(1));

        // 4. Poke the device. (Memory ordering caveat — see "Known
        //    weaknesses." Fine on QEMU.)
        write_reg(base, REG_QUEUE_NOTIFY, QUEUE_TX);

        // 5. Spin until the device confirms our descriptor is done.
        while (&raw const TX_QUEUE.used.idx).read_volatile() == used_idx_before {}
    }
}
