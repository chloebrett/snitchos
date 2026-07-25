//! `UdpFrameSink` — a [`FrameSink`](crate::sink::FrameSink) that batches
//! COBS-framed frames into MTU-sized UDP datagrams and hands each to a
//! [`NetDevice`](kernel_net::NetDevice).
//!
//! The telemetry payload is the exact wire format the UART path uses
//! ([`protocol::wire_encode`]); UDP only changes the envelope, and COBS is what
//! lets many frames share one datagram while staying independently decodable.
//! See `docs/network-telemetry-design.md` Decision 2. Alloc-free and
//! non-blocking: a full transmit path drops the batch and counts it, never
//! stalls the kernel — the same discipline as the alloc and IRQ deferred paths.

use crate::sink::FrameSink;
use kernel_net::{NetConfig, NetDevice, build_udp_datagram};
use protocol::{Frame, wire_encode};

/// Standard Ethernet MTU (the IPv4 total-length ceiling).
const MTU: usize = 1500;
/// Room for the batch payload after the IPv4 (20) + UDP (8) headers.
const MAX_BATCH: usize = MTU - 20 - 8;
/// A whole Ethernet frame: 14-byte header + MTU.
const DATAGRAM_MAX: usize = 14 + MTU;

/// Accumulates COBS-framed frames into a batch and flushes each full batch as
/// one UDP datagram through `D`.
pub struct UdpFrameSink<D: NetDevice> {
    config: NetConfig,
    device: D,
    batch: [u8; MAX_BATCH],
    batch_len: usize,
    dropped: u64,
}

impl<D: NetDevice> UdpFrameSink<D> {
    #[must_use]
    pub fn new(config: NetConfig, device: D) -> Self {
        Self { config, device, batch: [0; MAX_BATCH], batch_len: 0, dropped: 0 }
    }

    /// Datagrams dropped because the transmit path was full when a batch was
    /// flushed. Non-zero means telemetry was lost — surfaced as `Frame::Dropped`
    /// by the caller.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Emit the accumulated batch as one datagram and clear it. The seam a
    /// future heartbeat/time-based drain would call; today only overflow (in
    /// [`FrameSink::emit`]) and the caller invoke it.
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

impl<D: NetDevice> FrameSink for UdpFrameSink<D> {
    fn emit(&mut self, frame: &Frame<'_>) {
        if let Ok(bytes) = wire_encode(frame, &mut self.batch[self.batch_len..]) {
            self.batch_len += bytes.len();
            return;
        }
        // Didn't fit in the remaining batch space: flush what's there, then retry
        // into an empty batch. A frame too big even alone is dropped and counted.
        self.flush();
        match wire_encode(frame, &mut self.batch) {
            Ok(bytes) => self.batch_len = bytes.len(),
            Err(_) => self.dropped += 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::rc::Rc;
    use alloc::vec::Vec;
    use core::cell::{Cell, RefCell};
    use kernel_net::TxFull;
    use protocol::SpanId;

    /// A `NetDevice` that records the frames it's handed and can be told to fail.
    /// Both are behind shared handles so the test can inspect / toggle them after
    /// the sink has taken the device by value.
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

    /// Strip the Ethernet(14) + IPv4(20) + UDP(8) = 42-byte headers and split the
    /// COBS payload back into individual wire frames (each ends at a `0x00`
    /// delimiter, inclusive) — the inverse of what the sink batched.
    fn payload_frames(datagram: &[u8]) -> Vec<Vec<u8>> {
        let payload = &datagram[42..];
        let mut frames = Vec::new();
        let mut start = 0;
        for (i, &b) in payload.iter().enumerate() {
            if b == 0 {
                frames.push(payload[start..=i].to_vec());
                start = i + 1;
            }
        }
        frames
    }

    #[test]
    fn a_batch_flushes_as_one_datagram_decoding_back_in_order() {
        let dev = MockNet::new();
        let mut sink = UdpFrameSink::new(test_config(), dev.clone());

        let frames = [
            Frame::SpanEnd { id: SpanId(1), t: 10 },
            Frame::SpanEnd { id: SpanId(2), t: 20 },
            Frame::SpanEnd { id: SpanId(3), t: 30 },
        ];
        for f in &frames {
            sink.emit(f);
        }
        sink.flush();

        let sent = dev.sent.borrow();
        assert_eq!(sent.len(), 1, "one batch → one datagram");
        let chunks = payload_frames(&sent[0]);
        assert_eq!(chunks.len(), 3, "all three frames in the datagram");
        for (chunk, expected) in chunks.into_iter().zip(frames) {
            let mut buf = chunk;
            let got: Frame = postcard::from_bytes_cobs(&mut buf).expect("decodes");
            assert_eq!(got, expected);
        }
    }

    #[test]
    fn an_overflowing_frame_flushes_first_and_never_splits_a_frame() {
        let dev = MockNet::new();
        let mut sink = UdpFrameSink::new(test_config(), dev.clone());

        // Two ~800-byte Log frames can't share a 1472-byte batch, so the second
        // forces the first to flush before it starts a fresh batch.
        let big = "x".repeat(800);
        let a = Frame::Log { msg: &big, task_id: 1, t: 100, hart_id: 0 };
        let b = Frame::Log { msg: &big, task_id: 2, t: 200, hart_id: 0 };
        sink.emit(&a);
        sink.emit(&b);
        sink.flush();

        let sent = dev.sent.borrow();
        assert_eq!(sent.len(), 2, "each big frame lands in its own datagram");
        for (datagram, expected) in sent.iter().zip([a, b]) {
            let chunks = payload_frames(datagram);
            assert_eq!(chunks.len(), 1, "one whole frame per datagram, never split");
            let mut buf = chunks.into_iter().next().unwrap();
            let got: Frame = postcard::from_bytes_cobs(&mut buf).expect("decodes");
            assert_eq!(got, expected);
        }
    }

    #[test]
    fn a_full_transmit_path_drops_and_counts_then_recovers() {
        let dev = MockNet::new();
        let mut sink = UdpFrameSink::new(test_config(), dev.clone());

        dev.fail.set(true);
        sink.emit(&Frame::SpanEnd { id: SpanId(1), t: 10 });
        sink.flush();
        assert_eq!(sink.dropped(), 1, "the dropped datagram is counted");
        assert!(dev.sent.borrow().is_empty(), "nothing reached the wire");

        dev.fail.set(false);
        sink.emit(&Frame::SpanEnd { id: SpanId(2), t: 20 });
        sink.flush();
        assert_eq!(sink.dropped(), 1, "a successful flush doesn't count a drop");
        assert_eq!(dev.sent.borrow().len(), 1, "the sink recovers after a drop");
    }
}
