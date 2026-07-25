//! Pure Ethernet II / IPv4 / UDP datagram construction for egress-only
//! telemetry (M2.5). No MMIO, no alloc, no `protocol` dependency — the payload
//! is an opaque byte slice supplied by the caller (a batch of COBS-framed
//! `Frame`s, in practice). The NIC drivers that put these datagrams on a wire
//! live in `kernel/`; this crate only decides the bytes.
//!
//! Egress-only and statically addressed: no ARP/DHCP/ICMP/TCP, UDP checksum
//! elided (`0`, valid over IPv4). See `docs/network-telemetry-design.md`
//! Decision 1.

#![no_std]
#![forbid(unsafe_code)]

#[cfg(test)]
extern crate alloc;

/// Static addressing for the one telemetry neighbour. All fields are known at
/// boot (from the `net=` bootarg); nothing is discovered.
pub struct NetConfig {
    pub src_mac: [u8; 6],
    pub src_ip: [u8; 4],
    pub dst_mac: [u8; 6],
    pub dst_ip: [u8; 4],
    pub src_port: u16,
    pub dst_port: u16,
}

/// The destination buffer could not hold the full datagram.
#[derive(Debug, PartialEq, Eq)]
pub struct BufferTooSmall;

/// The device's transmit path had no room; the caller drops and counts.
#[derive(Debug, PartialEq, Eq)]
pub struct TxFull;

/// A hardware transmit path for complete Ethernet frames. Egress-only: the
/// telemetry stack never receives. Impls (virtio-net, GMAC) live in `kernel/`.
pub trait NetDevice {
    /// # Errors
    /// [`TxFull`] if the device's transmit ring has no free slot.
    fn send(&mut self, frame: &[u8]) -> Result<(), TxFull>;
}

/// RFC 1071 ones-complement checksum over `header` (16-bit words, big-endian),
/// as used for the IPv4 header check. `header` should have even length.
#[must_use]
pub fn ip_checksum(header: &[u8]) -> u16 {
    let sum: u32 = header
        .chunks(2)
        .map(|w| (u32::from(w[0]) << 8) | w.get(1).copied().map_or(0, u32::from))
        .sum();
    let folded = (sum & 0xffff) + (sum >> 16);
    let folded = (folded & 0xffff) + (folded >> 16);
    !(folded as u16)
}

/// Build one egress UDP datagram — Ethernet II + IPv4 + UDP + `payload` — into
/// `buf`, returning the written prefix.
///
/// # Errors
/// [`BufferTooSmall`] if `buf` cannot hold `42 + payload.len()` bytes.
pub fn build_udp_datagram<'a>(
    config: &NetConfig,
    payload: &[u8],
    buf: &'a mut [u8],
) -> Result<&'a [u8], BufferTooSmall> {
    const ETH: usize = 14;
    const IP: usize = 20;
    const UDP: usize = 8;

    let total = ETH + IP + UDP + payload.len();
    if buf.len() < total {
        return Err(BufferTooSmall);
    }
    let ip_total = (IP + UDP + payload.len()) as u16;
    let udp_total = (UDP + payload.len()) as u16;

    buf[0..6].copy_from_slice(&config.dst_mac);
    buf[6..12].copy_from_slice(&config.src_mac);
    buf[12..14].copy_from_slice(&0x0800u16.to_be_bytes());

    buf[14] = 0x45;
    buf[15] = 0x00;
    buf[16..18].copy_from_slice(&ip_total.to_be_bytes());
    buf[18..20].copy_from_slice(&0u16.to_be_bytes());
    buf[20..22].copy_from_slice(&0u16.to_be_bytes());
    buf[22] = 64;
    buf[23] = 17;
    buf[24..26].copy_from_slice(&0u16.to_be_bytes());
    buf[26..30].copy_from_slice(&config.src_ip);
    buf[30..34].copy_from_slice(&config.dst_ip);
    let checksum = ip_checksum(&buf[14..34]);
    buf[24..26].copy_from_slice(&checksum.to_be_bytes());

    buf[34..36].copy_from_slice(&config.src_port.to_be_bytes());
    buf[36..38].copy_from_slice(&config.dst_port.to_be_bytes());
    buf[38..40].copy_from_slice(&udp_total.to_be_bytes());
    buf[40..42].copy_from_slice(&0u16.to_be_bytes());

    buf[42..total].copy_from_slice(payload);

    Ok(&buf[..total])
}

/// Batch payload capacity: the 1500-byte Ethernet MTU less the 20-byte IPv4 and
/// 8-byte UDP headers (`1500 - 20 - 8`).
pub const MAX_BATCH: usize = 1472;
/// Scratch for one whole Ethernet frame: the 14-byte Ethernet header over a full
/// 1500-byte MTU payload (`14 + 1500`). The hard ceiling — a batch that would
/// exceed it fails to build and is dropped, never fragmented.
const DATAGRAM_MAX: usize = 1514;

/// Batches already-encoded (COBS) frame bytes into MTU-sized UDP datagrams and
/// flushes each full batch through a [`NetDevice`]. Alloc-free and non-blocking:
/// a full transmit path drops the batch and counts it, never stalls the caller —
/// the same discipline as the kernel's alloc and IRQ deferred paths.
///
/// This is the byte-level core the whole telemetry TX path shares: the kernel's
/// `UdpTx` pushes bytes it already encoded, and `kernel_obs`'s `UdpFrameSink`
/// wraps this with a `wire_encode` step. One batcher, one datagram format. See
/// `docs/network-telemetry-design.md`.
pub struct UdpBatcher<D: NetDevice> {
    config: NetConfig,
    device: D,
    batch: [u8; MAX_BATCH],
    batch_len: usize,
    dropped: u64,
}

impl<D: NetDevice> UdpBatcher<D> {
    #[must_use]
    pub fn new(config: NetConfig, device: D) -> Self {
        Self { config, device, batch: [0; MAX_BATCH], batch_len: 0, dropped: 0 }
    }

    /// Datagrams dropped because the transmit path was full when a batch was
    /// flushed, plus any single frame too large to ever fit a datagram.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Append one already-encoded frame's `bytes` to the current batch. A frame
    /// that would overflow the batch flushes it first (never split); one too
    /// large to ever fit is dropped and counted.
    pub fn push_bytes(&mut self, bytes: &[u8]) {
        if bytes.len() > MAX_BATCH {
            self.dropped += 1;
            return;
        }
        if self.batch_len + bytes.len() > MAX_BATCH {
            self.flush();
        }
        self.batch[self.batch_len..self.batch_len + bytes.len()].copy_from_slice(bytes);
        self.batch_len += bytes.len();
    }

    /// Emit the accumulated batch as one datagram and clear it. The seam a
    /// future heartbeat/time-based drain would call.
    pub fn flush(&mut self) {
        if self.batch_len == 0 {
            return;
        }
        let mut datagram = [0u8; DATAGRAM_MAX];
        match build_udp_datagram(&self.config, &self.batch[..self.batch_len], &mut datagram) {
            Ok(bytes) if self.device.send(bytes).is_ok() => {}
            _ => self.dropped += 1,
        }
        self.batch_len = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::rc::Rc;
    use alloc::vec::Vec;
    use core::cell::{Cell, RefCell};

    /// A `NetDevice` that records sent frames and can be told to fail — behind
    /// shared handles so the test inspects/toggles them after `UdpBatcher` has
    /// taken the device by value.
    #[derive(Clone)]
    struct MockNet {
        sent: Rc<RefCell<Vec<Vec<u8>>>>,
        fail: Rc<Cell<bool>>,
    }

    impl MockNet {
        fn new() -> Self {
            Self { sent: Rc::new(RefCell::new(Vec::new())), fail: Rc::new(Cell::new(false)) }
        }
    }

    impl NetDevice for MockNet {
        fn send(&mut self, frame: &[u8]) -> Result<(), TxFull> {
            if self.fail.get() {
                return Err(TxFull);
            }
            self.sent.borrow_mut().push(frame.to_vec());
            Ok(())
        }
    }

    #[test]
    fn push_bytes_batches_and_flushes_as_one_datagram() {
        let dev = MockNet::new();
        let mut b = UdpBatcher::new(test_config(), dev.clone());
        b.push_bytes(&[1, 2, 3]);
        b.push_bytes(&[4, 5]);
        b.push_bytes(&[6]);
        b.flush();

        assert_eq!(b.dropped(), 0, "a clean run drops nothing");
        let sent = dev.sent.borrow();
        assert_eq!(sent.len(), 1, "one batch → one datagram");
        assert_eq!(&sent[0][42..], &[1, 2, 3, 4, 5, 6], "payload is the concatenated frames");
    }

    #[test]
    fn an_overflowing_push_flushes_the_batch_first() {
        let dev = MockNet::new();
        let mut b = UdpBatcher::new(test_config(), dev.clone());
        // Two ~800-byte frames can't share a 1472-byte batch.
        let big = [0x5au8; 800];
        b.push_bytes(&big);
        b.push_bytes(&big);
        b.flush();

        let sent = dev.sent.borrow();
        assert_eq!(sent.len(), 2, "each frame lands in its own datagram");
        assert_eq!(&sent[0][42..], &big);
        assert_eq!(&sent[1][42..], &big);
    }

    #[test]
    fn a_frame_exactly_filling_the_batch_is_accepted() {
        // MAX_BATCH bytes is the largest single frame that still fits — the
        // boundary between "batched" and "dropped as too large".
        let dev = MockNet::new();
        let mut b = UdpBatcher::new(test_config(), dev.clone());
        let exact = [0x11u8; MAX_BATCH];
        b.push_bytes(&exact);
        b.flush();

        assert_eq!(b.dropped(), 0, "an exactly-max frame is not too large");
        let sent = dev.sent.borrow();
        assert_eq!(sent.len(), 1);
        assert_eq!(&sent[0][42..], &exact[..]);
    }

    #[test]
    fn two_frames_summing_to_exactly_the_batch_share_one_datagram() {
        // 1000 + 472 == MAX_BATCH: the second fits exactly, so no flush happens
        // between them — the boundary of the overflow check.
        let dev = MockNet::new();
        let mut b = UdpBatcher::new(test_config(), dev.clone());
        let a = [0xAAu8; 1000];
        let c = [0xBBu8; MAX_BATCH - 1000];
        b.push_bytes(&a);
        b.push_bytes(&c);
        b.flush();

        let sent = dev.sent.borrow();
        assert_eq!(sent.len(), 1, "an exact fit does not flush early");
        assert_eq!(&sent[0][42..42 + 1000], &a[..]);
        assert_eq!(&sent[0][42 + 1000..], &c[..]);
    }

    #[test]
    fn bytes_too_large_for_a_datagram_are_dropped_and_counted() {
        let dev = MockNet::new();
        let mut b = UdpBatcher::new(test_config(), dev.clone());
        b.push_bytes(&[0u8; 2000]);
        b.flush();

        assert_eq!(b.dropped(), 1, "the un-sendable frame is counted");
        assert!(dev.sent.borrow().is_empty(), "nothing reached the wire");
    }

    #[test]
    fn a_full_transmit_path_drops_and_counts_then_recovers() {
        let dev = MockNet::new();
        let mut b = UdpBatcher::new(test_config(), dev.clone());

        dev.fail.set(true);
        b.push_bytes(&[1, 2, 3]);
        b.flush();
        assert_eq!(b.dropped(), 1, "the dropped datagram is counted");
        assert!(dev.sent.borrow().is_empty());

        dev.fail.set(false);
        b.push_bytes(&[4, 5, 6]);
        b.flush();
        assert_eq!(b.dropped(), 1, "a successful flush doesn't count a drop");
        assert_eq!(dev.sent.borrow().len(), 1, "the batcher recovers after a drop");
    }

    // A fixed neighbour used across the datagram tests. Golden bytes below were
    // computed from this config with an independent Python reference, not by
    // hand — so the test pins the wire layout, not my arithmetic.
    fn test_config() -> NetConfig {
        NetConfig {
            src_mac: [0x52, 0x54, 0x00, 0x12, 0x34, 0x56],
            src_ip: [10, 0, 0, 2],
            dst_mac: [0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff],
            dst_ip: [10, 0, 0, 1],
            src_port: 40000,
            dst_port: 9000,
        }
    }

    // The IPv4 header for `test_config()` + a 6-byte payload, with the checksum
    // field zeroed — the exact input the checksum is computed over.
    const IP_HEADER_NO_CHECKSUM: [u8; 20] = [
        0x45, 0x00, 0x00, 0x22, 0x00, 0x00, 0x00, 0x00, 0x40, 0x11, 0x00, 0x00, 0x0a, 0x00, 0x00,
        0x02, 0x0a, 0x00, 0x00, 0x01,
    ];

    // Full datagram: Ethernet II (14) + IPv4 (20) + UDP (8) + b"snitch" (6).
    const GOLDEN: [u8; 48] = [
        0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff, 0x52, 0x54, 0x00, 0x12, 0x34, 0x56, 0x08, 0x00, 0x45,
        0x00, 0x00, 0x22, 0x00, 0x00, 0x00, 0x00, 0x40, 0x11, 0x66, 0xc9, 0x0a, 0x00, 0x00, 0x02,
        0x0a, 0x00, 0x00, 0x01, 0x9c, 0x40, 0x23, 0x28, 0x00, 0x0e, 0x00, 0x00, 0x73, 0x6e, 0x69,
        0x74, 0x63, 0x68,
    ];

    #[test]
    fn ip_checksum_matches_rfc1071_reference() {
        assert_eq!(ip_checksum(&IP_HEADER_NO_CHECKSUM), 0x66c9);
    }

    #[test]
    fn ip_checksum_folds_end_around_carries() {
        // 0xffff + 0xffff + 0x0001 = 0x1ffff, which carries twice: the first
        // fold produces 0x10000, the second reduces it to 1. Exercises the
        // carry-fold arithmetic that a header summing under 0x10000 never
        // touches (verified against the Python reference: 0xfffe).
        assert_eq!(ip_checksum(&[0xff, 0xff, 0xff, 0xff, 0x00, 0x01]), 0xfffe);
    }

    #[test]
    fn a_complete_header_checksums_to_zero() {
        // The defining property of the IP checksum: fold the header *including*
        // its own checksum field and the result is zero. Independent of the
        // magic constant above — a receiver's validity check.
        let mut header = IP_HEADER_NO_CHECKSUM;
        let ck = ip_checksum(&header).to_be_bytes();
        header[10] = ck[0];
        header[11] = ck[1];
        assert_eq!(ip_checksum(&header), 0);
    }

    #[test]
    fn build_udp_datagram_matches_golden() {
        let mut buf = [0u8; 64];
        let got = build_udp_datagram(&test_config(), b"snitch", &mut buf).expect("fits");
        assert_eq!(got, &GOLDEN);
    }

    #[test]
    fn buffer_one_byte_too_small_is_an_error() {
        // Needs 42 + 6 = 48 bytes; give it 47.
        let mut buf = [0u8; 47];
        assert_eq!(
            build_udp_datagram(&test_config(), b"snitch", &mut buf),
            Err(BufferTooSmall)
        );
    }

    #[test]
    fn an_exactly_sized_buffer_succeeds() {
        // The boundary: 42 + 6 = 48 bytes is an exact fit, not too small.
        let mut buf = [0u8; 48];
        let got = build_udp_datagram(&test_config(), b"snitch", &mut buf).expect("exact fit");
        assert_eq!(got, &GOLDEN);
    }
}
