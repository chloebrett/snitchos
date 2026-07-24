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

#[cfg(test)]
mod tests {
    use super::*;

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
}
