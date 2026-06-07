//! toy-virtqueue — a standalone virtio descriptor-ring playground.
//!
//! This is how the kernel hands data to a device (`kernel/src/virtio_console.rs`).
//! A *virtqueue* is three pieces of shared memory:
//!
//! - **descriptor table** — array of `{addr, len, flags, next}`. Each entry
//!   describes one buffer: "there are `len` bytes at physical address `addr`."
//! - **available ring** (driver → device) — the driver pushes descriptor
//!   indices it wants processed, then bumps `avail.idx`.
//! - **used ring** (device → driver) — the device pushes the indices it has
//!   finished, with how many bytes it touched, then bumps `used.idx`.
//!
//! The transmit protocol: fill a descriptor → publish its index in the avail
//! ring → (notify) → the device DMAs the buffer and posts to the used ring.
//!
//! ## The one rule this toy exists to teach
//!
//! **A device has no MMU.** When it reads your buffer, it uses the `addr` in
//! the descriptor as a *raw physical address* — it does NOT walk a page table.
//! So the driver must put a **physical** address in the descriptor, never a
//! virtual one. Hand it a VA and it DMAs whatever physical memory that
//! bit-pattern happens to name → silent corruption.
//!
//! We model this concretely: [`Machine::phys`] is a `Vec<u8>` standing in for
//! physical RAM, and the fake device indexes it *directly* by `addr`. Buffers
//! are referred to by a virtual address (`pa + KERNEL_OFFSET`); the driver
//! must translate with [`va_to_pa`] before writing the descriptor. Exactly the
//! `va_to_pa` site in `virtio_console::transmit`.

/// Higher-half offset, like the kernel's `KERNEL_OFFSET`. A buffer at physical
/// address `p` is named by virtual address `p + KERNEL_OFFSET`.
pub const KERNEL_OFFSET: usize = 0x1_0000_0000; // 4 GiB

/// Translate a kernel VA to its physical address — strip `KERNEL_OFFSET` for
/// higher-half VAs, pass identity-range addresses through. Mirrors
/// `kernel-core::mmu::va_to_pa`.
pub fn va_to_pa(va: usize) -> usize {
    if va >= KERNEL_OFFSET {
        va - KERNEL_OFFSET
    } else {
        va
    }
}

/// Number of slots in each ring (a power of two; the index wraps mod this).
pub const QSIZE: usize = 4;

/// One descriptor: a buffer at physical `addr` of `len` bytes. `flags`/`next`
/// support chaining in real virtio; this toy only uses single buffers
/// (`flags = 0`), but they're kept for fidelity with `VirtqDesc`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Desc {
    pub addr: u64,
    pub len: u32,
    pub flags: u16,
    pub next: u16,
}

/// Driver → device ring: descriptor indices the device should process.
#[derive(Clone, Debug)]
pub struct Avail {
    pub idx: u16,
    pub ring: [u16; QSIZE],
}

/// Device → driver ring: one entry per completed descriptor.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UsedElem {
    pub id: u32,
    pub len: u32,
}

#[derive(Clone, Debug)]
pub struct Used {
    pub idx: u16,
    pub ring: [UsedElem; QSIZE],
}

/// A tiny "machine": physical memory + one virtqueue + a fake device sink.
pub struct Machine {
    /// Stands in for physical RAM. The device indexes this *directly* by the
    /// descriptor's `addr` — no translation, because devices have no MMU.
    phys: Vec<u8>,
    next_free: usize,

    pub desc: [Desc; QSIZE],
    pub avail: Avail,
    pub used: Used,

    /// What the fake device has DMA'd out so far (the "wire").
    transmitted: Vec<u8>,
}

impl Machine {
    pub fn new(phys_bytes: usize) -> Self {
        Self {
            phys: vec![0u8; phys_bytes],
            next_free: 0,
            desc: [Desc::default(); QSIZE],
            avail: Avail { idx: 0, ring: [0; QSIZE] },
            used: Used { idx: 0, ring: [UsedElem::default(); QSIZE] },
            transmitted: Vec::new(),
        }
    }

    /// Copy `bytes` into physical memory and return the **virtual address**
    /// the driver would hold for it (`pa + KERNEL_OFFSET`). Bump-allocated.
    pub fn place_buffer(&mut self, bytes: &[u8]) -> usize {
        let pa = self.next_free;
        self.phys[pa..pa + bytes.len()].copy_from_slice(bytes);
        self.next_free += bytes.len();
        pa + KERNEL_OFFSET
    }

    /// Everything the device has transmitted, in order.
    pub fn transmitted(&self) -> &[u8] {
        &self.transmitted
    }

    // -------------------------------------------------------------------
    // EXERCISE 1 — publish a buffer (the DRIVER side).
    //
    // The driver wants the device to send `len` bytes of the buffer whose
    // *virtual* address is `va`. Steps (mirror `virtio_console::transmit`):
    //   1. Choose a descriptor slot. Use `self.avail.idx as usize % QSIZE`
    //      as the descriptor index `id`.
    //   2. Write `self.desc[id]` = a Desc pointing at the buffer. THE RULE:
    //      `addr` must be the PHYSICAL address — `va_to_pa(va)` — not `va`.
    //      `len = len`, `flags = 0`, `next = 0`.
    //   3. Push `id` into the available ring at `avail.idx % QSIZE`, then
    //      bump `avail.idx` (wrapping_add(1)).
    //
    // The device is notified implicitly (this toy has no MMIO notify
    // register — `device_poll` stands in for the device waking up).
    // -------------------------------------------------------------------
    pub fn publish(&mut self, va: usize, len: usize) {
        let _ = (va, len);
        todo!("EXERCISE 1: fill a descriptor with the PHYSICAL addr + push to avail")
    }

    // -------------------------------------------------------------------
    // EXERCISE 2 — process available buffers (the DEVICE side).
    //
    // The device drains everything the driver has made available. Loop while
    // `self.used.idx != self.avail.idx`:
    //   1. The next descriptor index is `self.avail.ring[used.idx % QSIZE]`.
    //   2. Read that descriptor. DMA the buffer out of PHYSICAL memory:
    //      `&self.phys[addr .. addr + len]` — indexing `phys` *directly* by
    //      the descriptor's `addr`, because the device has no MMU. Append
    //      those bytes to `self.transmitted`.
    //   3. Post completion: `self.used.ring[used.idx % QSIZE] =
    //      UsedElem { id, len }`, then bump `used.idx` (wrapping_add(1)).
    //
    // Note step 2 is what makes the VA/PA rule matter: if the driver wrote a
    // VA into `addr`, this direct index hits the wrong bytes (or panics).
    // -------------------------------------------------------------------
    pub fn device_poll(&mut self) {
        todo!("EXERCISE 2: drain avail ring, DMA from phys[addr], post to used")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn va_to_pa_strips_the_offset() {
        assert_eq!(va_to_pa(KERNEL_OFFSET), 0);
        assert_eq!(va_to_pa(KERNEL_OFFSET + 0x40), 0x40);
        assert_eq!(va_to_pa(0x40), 0x40); // identity-range passes through
    }

    // ---- Exercise 1: publish (driver) ---------------------------------

    #[test]
    fn publish_writes_the_physical_address_into_the_descriptor() {
        let mut m = Machine::new(4096);
        let va = m.place_buffer(b"hi");
        m.publish(va, 2);

        let d = m.desc[0];
        assert_eq!(d.addr, va_to_pa(va) as u64, "descriptor must hold the PA");
        assert_ne!(d.addr, va as u64, "a VA in the descriptor would be the bug");
        assert_eq!(d.len, 2);
    }

    #[test]
    fn publish_pushes_into_the_available_ring() {
        let mut m = Machine::new(4096);
        let va = m.place_buffer(b"hi");
        m.publish(va, 2);
        assert_eq!(m.avail.idx, 1);
        assert_eq!(m.avail.ring[0], 0); // descriptor id 0 made available
    }

    // ---- Exercise 2: device_poll (device) -----------------------------

    #[test]
    fn round_trip_transmits_the_buffer() {
        let mut m = Machine::new(4096);
        let va = m.place_buffer(b"hello");
        m.publish(va, 5);
        m.device_poll();
        assert_eq!(m.transmitted(), b"hello");
        assert_eq!(m.used.idx, 1, "device should post one completion");
    }

    #[test]
    fn device_poll_with_nothing_available_is_a_noop() {
        let mut m = Machine::new(4096);
        m.device_poll();
        assert_eq!(m.transmitted(), b"");
        assert_eq!(m.used.idx, 0);
    }

    #[test]
    fn multiple_buffers_transmit_in_order() {
        let mut m = Machine::new(4096);
        let a = m.place_buffer(b"foo");
        let b = m.place_buffer(b"bar");
        m.publish(a, 3);
        m.publish(b, 3);
        m.device_poll();
        assert_eq!(m.transmitted(), b"foobar");
        assert_eq!(m.used.idx, 2);
    }

    #[test]
    fn available_ring_wraps_past_qsize() {
        // Publish + drain more than QSIZE buffers; the ring index must wrap
        // cleanly (avail.idx is u16, slot is idx % QSIZE).
        let mut m = Machine::new(4096);
        for i in 0..(QSIZE as u16 + 2) {
            let va = m.place_buffer(&[b'a' + i as u8]);
            m.publish(va, 1);
            m.device_poll(); // drain each so descriptors are reusable
        }
        assert_eq!(m.transmitted(), b"abcdef");
        assert_eq!(m.used.idx, QSIZE as u16 + 2);
    }

    // ---- Property: any sequence of buffers round-trips byte-for-byte ----
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn published_buffers_arrive_concatenated(
            payloads in prop::collection::vec(
                prop::collection::vec(any::<u8>(), 1..16),
                0..QSIZE, // keep within ring capacity per drain
            ),
        ) {
            let mut m = Machine::new(64 * 1024);
            let mut expected = Vec::new();
            for p in &payloads {
                let va = m.place_buffer(p);
                m.publish(va, p.len());
                expected.extend_from_slice(p);
            }
            m.device_poll();
            prop_assert_eq!(m.transmitted(), &expected[..]);
        }
    }
}
