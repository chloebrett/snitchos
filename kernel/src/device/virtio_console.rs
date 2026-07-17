//! virtio-console (DeviceID 3 over virtio-mmio). Telemetry channel —
//! separate from the NS16550A used for `println!` text.
//!
//! Discovers the device by walking the DTB, drives the full virtio
//! handshake + virtqueue setup, and transmits telemetry frames on the TX
//! queue. Frame bytes are staged through a static `TX_STAGING` buffer so
//! the descriptor carries a `KERNEL_OFFSET`-range PA that `va_to_pa` can
//! translate (heap VAs can't — see the `send` staging note).

use fdt::Fdt;

/// The virtio-mmio register map, status bits, spec constants, and the
/// virtqueue layout structs all live in `kernel_devices::virtio` (pure
/// data, host-tested). The kernel owns the statics, the volatile
/// register access, and the handshake driving them.
use kernel_devices::virtio::{
    DEVICE_ID_CONSOLE, MAGIC, QSIZE, QUEUE_RX, QUEUE_TX, REG_DEVICE_ID, REG_MAGIC_VALUE,
    REG_QUEUE_NOTIFY, REG_VERSION, VERSION, VirtqAvail, VirtqDesc, VirtqUsed, VirtqUsedElem,
    Virtqueue,
};

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

/// Build a `QueueConfig` for one virtqueue: translate the three ring
/// regions' VAs to physical addresses (the device has no MMU) so the
/// host-tested `kernel_devices::virtio::handshake` can install them.
///
/// # Safety
///
/// `queue` must outlive the device's use of it (in practice `'static`,
/// which is why we take a pointer not a reference — the device writes
/// to the used ring and we don't want to imply Rust's aliasing rules
/// cover device accesses).
unsafe fn queue_config(sel: u32, queue: *const Virtqueue) -> kernel_devices::virtio::QueueConfig {
    // `va_to_pa` is a no-op at identity PC and strips KERNEL_OFFSET once
    // the kernel runs higher-half.
    // SAFETY: `queue` is a 'static Virtqueue; taking the field addresses
    // and translating them to PAs is sound.
    unsafe {
        kernel_devices::virtio::QueueConfig {
            sel,
            desc_pa: crate::mmu::va_to_pa(&raw const (*queue).desc as usize) as u64,
            avail_pa: crate::mmu::va_to_pa(&raw const (*queue).avail as usize) as u64,
            used_pa: crate::mmu::va_to_pa(&raw const (*queue).used as usize) as u64,
        }
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

/// Length of the per-console TX staging buffer.
const TX_STAGING_LEN: usize = 256;

/// The mutex-guarded state for one virtio-console: its MMIO base plus
/// the TX staging buffer.
///
/// The buffer lives **inside** the mutex on purpose. The device's
/// descriptor needs a *physical* address; frames built on heap-backed
/// task stacks have heap-range VAs that `mmu::va_to_pa` won't translate,
/// so `send` copies them into this `.bss`-resident buffer (whose VA
/// `va_to_pa` strips correctly) before handing it to the device.
///
/// Guarding the buffer with the same lock as `base` is what makes the
/// stage→transmit critical section exclusive across concurrent senders
/// (two harts emitting at once). Crucially, `TxStaging` is **not**
/// `Copy`, so the old footgun `let base = *handle.lock();` — which
/// copied `base` out and dropped the guard early, leaving the buffer
/// unprotected — no longer compiles. See plans/legacy/tx-staging-cross-hart-race.md.
pub struct TxStaging {
    base: usize,
    buf: [u8; TX_STAGING_LEN],
}

/// The global virtio-console handle. Set once at boot via `init`. The
/// lock serializes concurrent senders (interrupts / SMP) across the
/// whole stage+transmit. Same lock pattern as the NS16550 `UART`.
pub static CONSOLE: crate::sync::Once<crate::sync::Mutex<TxStaging>> = crate::sync::Once::new();

/// Errors that can arise during virtio-console initialization: either
/// the kernel-side DTB discovery failed (`NotFound`), or the pure
/// `kernel_devices::virtio::handshake` did (`Handshake`).
#[derive(Debug)]
pub enum InitError {
    /// No virtio-mmio slot advertised DeviceID 3 (console).
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

/// A virtio-mmio device addressed by its higher-half MMIO base. Adapts
/// the kernel's volatile register access to the host-testable
/// `MmioTransport` trait so the feature handshake can live in kernel-core.
struct MmioConsole {
    base: usize,
}

impl kernel_devices::virtio::MmioTransport for MmioConsole {
    fn read_reg(&self, offset: usize) -> u32 {
        // SAFETY: `base` is a discovered virtio-mmio base (`find_console_base`)
        // and `offset` is a register within that device's region.
        unsafe { read_reg(self.base, offset) }
    }

    fn write_reg(&mut self, offset: usize, value: u32) {
        // SAFETY: as above; the handshake only writes valid register values.
        unsafe { write_reg(self.base, offset, value) }
    }
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
    CONSOLE.call_once(|| crate::sync::Mutex::new(TxStaging { base, buf: [0u8; TX_STAGING_LEN] }));
    Ok(())
}

/// Send a buffer of bytes out the kernel's virtio-console. Silently
/// no-ops if `init` hasn't completed (matches the println macro's
/// "pre-init bytes are lost" behavior).
///
/// The `CONSOLE` guard is held across the entire stage+transmit. Because
/// the staging buffer lives *inside* the guarded `TxStaging`, the lock
/// structurally covers both the copy and the device handoff — there's no
/// way to touch the buffer without holding it.
pub fn send(bytes: &[u8]) {
    let Some(handle) = CONSOLE.get() else {
        return;
    };
    let mut staging = handle.lock();
    let base = staging.base;
    // SAFETY: the guard is held for the whole `stage_and_emit` call, so
    // this hart is the single writer to the staging buffer and the sole
    // driver of the virtqueue while `transmit` runs. `staged` points into
    // the `.bss`-resident buffer, so `transmit`'s `va_to_pa` is correct.
    kernel_devices::virtio::stage_and_emit(&mut staging.buf, bytes, |staged| unsafe {
        transmit(base, staged);
    });
}

/// Best-effort, **non-blocking** send for the panic path: emit `bytes` iff the
/// console is up *and* its lock is free right now. Returns `true` if the frame
/// was staged+transmitted, `false` if `init` hasn't run or the lock is held.
///
/// A panic can fire from anywhere — including a hart that's *mid-[`send`]* and
/// already holds this lock. A blocking `lock()` there would deadlock, so this
/// uses [`try_lock`](crate::sync::Mutex::try_lock) and simply gives up on
/// contention (the panic message is already on the emergency UART regardless).
/// Dropping the frame on contention is only acceptable *because* it's the panic
/// path — hence the `_panic` suffix; normal telemetry must never silently drop.
///
/// Reusing the in-mutex `staging.buf` (not a separate buffer) is sound precisely
/// because `try_lock` succeeding proves no one else is mid-stage: the same lock
/// guards the buffer and `TX_QUEUE`, so a free lock means both are in a
/// consistent, idle state. `transmit` spins to completion before we release the
/// guard, so the device has finished reading the buffer by the time we return —
/// fine on a halting kernel.
#[must_use]
pub fn try_send_panic(bytes: &[u8]) -> bool {
    let Some(handle) = CONSOLE.get() else {
        return false;
    };
    // Retry `try_lock` rather than a single shot: a *peer* hart emitting telemetry
    // holds this lock for a full device round-trip (the `transmit` spin), which
    // under load is most of the time — so one `try_lock` usually loses and drops
    // the panic frame. Retrying across the peer's release windows reliably catches
    // a gap. Bounded (never a blocking `lock()`) so that if *this* hart panicked
    // while already holding the lock, we give up instead of self-deadlocking —
    // the panic message is already on the emergency UART regardless.
    for _ in 0..PANIC_SEND_TRY_LOCK_SPINS {
        let Some(mut staging) = handle.try_lock() else {
            core::hint::spin_loop();
            continue;
        };
        let base = staging.base;
        // SAFETY: `try_lock` succeeded, so this hart is the exclusive holder — the
        // sole writer to `staging.buf` and sole driver of `TX_QUEUE` for this
        // critical section. `staged` points into the `.bss`-resident buffer, so
        // `transmit`'s `va_to_pa` is correct.
        kernel_devices::virtio::stage_and_emit(&mut staging.buf, bytes, |staged| unsafe {
            transmit(base, staged);
        });
        return true;
    }
    false
}

/// Bound on the panic send's `try_lock` retries. Large enough to outlast a busy
/// peer hart's telemetry (many device round-trips, even under heavy host load),
/// so the panic frame reliably lands; finite so a self-panic-while-holding-the-
/// lock gives up (~a few hundred ms of spin, on a kernel that's already halting)
/// instead of hanging. Best-effort, not a guarantee — the UART message is the
/// backstop.
const PANIC_SEND_TRY_LOCK_SPINS: u32 = 200_000_000;

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
    let mut dev = MmioConsole { base };
    // virtio-console requires BOTH port 0 RX (queue 0) and TX (queue 1)
    // configured, even though we never receive — the device may silently
    // drop our TX otherwise.
    // SAFETY: RX_QUEUE / TX_QUEUE are 'static.
    let queues = unsafe {
        [
            queue_config(QUEUE_RX, &raw const RX_QUEUE),
            queue_config(QUEUE_TX, &raw const TX_QUEUE),
        ]
    };
    kernel_devices::virtio::handshake(&mut dev, &queues, QSIZE).map_err(InitError::from)
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
        //    bump avail.idx. Ring slot + next index (with their distinct
        //    wrap points) come from the host-tested pure helper.
        let enq = kernel_devices::virtio::avail_enqueue(avail_idx_before, QSIZE);
        (&raw mut TX_QUEUE.avail.ring[enq.ring_slot]).write_volatile(0);
        (&raw mut TX_QUEUE.avail.idx).write_volatile(enq.next_idx);

        // 4. Make the descriptor + ring writes globally visible BEFORE
        //    the device thread sees the notify-induced wake. Required
        //    under multi-thread TCG (one host thread per emulated CPU
        //    + a device thread) and real hardware. fence(Release) lowers
        //    to RISC-V `fence rw,w`.
        core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
        write_reg(base, REG_QUEUE_NOTIFY, QUEUE_TX);

        // 5. Spin until the device confirms our descriptor is done.
        while !kernel_devices::virtio::used_advanced(
            used_idx_before,
            (&raw const TX_QUEUE.used.idx).read_volatile(),
        ) {}
    }
}
