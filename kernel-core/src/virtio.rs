//! virtio data definitions — the `#[repr(C)]` virtqueue structures and
//! spec constants shared between the kernel driver and host tests. Pure
//! layout: no MMIO, no `unsafe`. The DMA wire contract lives here so
//! the layout-pinning tests run on the host; the kernel side owns the
//! statics, the volatile register access, and the handshake.

/// Number of descriptors in our TX queue. Power of 2 required by spec.
pub const QSIZE: usize = 8;

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
    use core::mem::{align_of, offset_of, size_of};

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
    fn virtqueue_is_16_byte_aligned() {
        // The descriptor table requires 16-byte alignment; the outer
        // align(16) on Virtqueue is what guarantees it for the whole
        // statically-allocated block.
        assert_eq!(align_of::<Virtqueue>(), 16);
    }
}
