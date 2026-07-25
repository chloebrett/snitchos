//! virtio-net TX protocol logic — the pure, host-tested half of the QEMU NIC
//! driver. No MMIO and no statics: the `static mut` transmit queue, the volatile
//! descriptor write, and the notify/poll live in `kernel/src/device/`. What's
//! here is what decides the bytes — the 12-byte virtio-net header prepend — and
//! the net-specific device/feature constants.
//!
//! Feature negotiation reuses [`crate::virtio::negotiate_features`]: it accepts
//! only `VIRTIO_F_VERSION_1`, which for net means declining every offload
//! feature (checksum, GSO, [`NET_F_MRG_RXBUF`]) — exactly what keeps the plain
//! [`NET_HDR_LEN`] header valid. The transmit sequencing reuses
//! [`crate::virtio::avail_enqueue`]. See `docs/network-telemetry-design.md`.

/// virtio-net device id (the virtio-mmio `DeviceID` register value).
pub const DEVICE_ID_NET: u32 = 1;

/// The plain virtio-net TX header: 12 zero bytes (no checksum/GSO offload, under
/// `VIRTIO_F_VERSION_1`). Prepended to every transmitted frame; the device reads
/// `[header || frame]` as one buffer.
pub const NET_HDR_LEN: usize = 12;

/// `VIRTIO_NET_F_MRG_RXBUF` (bit 15). We decline it — merging RX buffers changes
/// the header layout the fixed [`NET_HDR_LEN`] assumes — and rely on
/// [`crate::virtio::negotiate_features`] accepting only `VIRTIO_F_VERSION_1` to
/// do so.
pub const NET_F_MRG_RXBUF: u64 = 1 << 15;

/// Stage an Ethernet `frame` for transmit — the 12-byte zero header followed by
/// the frame — into `staging`, then hand the staged prefix to `emit`. Returns
/// the staged length (`NET_HDR_LEN + frame.len()`, clamped to `staging`).
/// Mirrors [`crate::virtio::stage_and_emit`], which the console TX uses; net
/// differs only by the header.
pub fn stage_net_tx<F: FnOnce(&[u8])>(staging: &mut [u8], frame: &[u8], emit: F) -> usize {
    let hdr = NET_HDR_LEN.min(staging.len());
    staging[..hdr].fill(0);
    let frame_len = frame.len().min(staging.len() - hdr);
    staging[hdr..hdr + frame_len].copy_from_slice(&frame[..frame_len]);
    let len = hdr + frame_len;
    emit(&staging[..len]);
    len
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::virtio::{AvailEnqueue, F_VERSION_1, QSIZE, avail_enqueue, negotiate_features};
    use alloc::vec::Vec;

    #[test]
    fn stage_net_tx_prepends_a_zero_header_then_the_frame() {
        // Pre-fill with 0xff so a header that isn't actively zeroed is caught.
        let mut staging = [0xffu8; 64];
        let frame = [0xaa, 0xbb, 0xcc, 0xdd];
        let mut emitted = Vec::new();
        let n = stage_net_tx(&mut staging, &frame, |staged| emitted.extend_from_slice(staged));

        assert_eq!(n, NET_HDR_LEN + frame.len());
        assert_eq!(emitted.len(), NET_HDR_LEN + frame.len());
        assert_eq!(&emitted[..NET_HDR_LEN], &[0u8; NET_HDR_LEN], "12-byte zero header");
        assert_eq!(&emitted[NET_HDR_LEN..], &frame, "frame follows the header");
    }

    #[test]
    fn stage_net_tx_clamps_a_frame_that_overflows_staging() {
        // staging holds the 12-byte header + only 4 more bytes; a 10-byte frame
        // must be clamped to 4, never written past the buffer. Exercises the
        // remaining-space arithmetic that a frame comfortably under capacity
        // leaves untouched.
        let mut staging = [0xffu8; NET_HDR_LEN + 4];
        let frame = [1u8; 10];
        let mut emitted = Vec::new();
        let n = stage_net_tx(&mut staging, &frame, |staged| emitted.extend_from_slice(staged));

        assert_eq!(n, NET_HDR_LEN + 4, "clamped to header + remaining space");
        assert_eq!(&emitted[..NET_HDR_LEN], &[0u8; NET_HDR_LEN]);
        assert_eq!(&emitted[NET_HDR_LEN..], &[1u8; 4], "only the fitting prefix");
    }

    #[test]
    fn net_f_mrg_rxbuf_is_the_spec_bit() {
        // A wire-protocol constant: VIRTIO_NET_F_MRG_RXBUF is feature bit 15.
        // Pinned so a mistyped bit is caught, since negotiation only ever checks
        // VERSION_1 and would silently accept a wrong value here.
        assert_eq!(NET_F_MRG_RXBUF, 0x8000);
    }

    #[test]
    fn net_negotiation_accepts_version_1_and_declines_mrg_rxbuf() {
        // A real virtio-net device offers offload features too; we take only
        // VERSION_1 so the plain 12-byte header always applies.
        assert_eq!(
            negotiate_features(F_VERSION_1 | NET_F_MRG_RXBUF),
            Some(F_VERSION_1)
        );
    }

    #[test]
    fn net_negotiation_rejects_a_device_without_version_1() {
        assert_eq!(negotiate_features(NET_F_MRG_RXBUF), None);
    }

    #[test]
    fn successive_tx_enqueues_advance_the_ring_by_one() {
        assert_eq!(avail_enqueue(0, QSIZE), AvailEnqueue { ring_slot: 0, next_idx: 1 });
        assert_eq!(avail_enqueue(1, QSIZE), AvailEnqueue { ring_slot: 1, next_idx: 2 });
    }
}
