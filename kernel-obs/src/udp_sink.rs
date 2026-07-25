//! `UdpFrameSink` — a [`FrameSink`] that encodes each frame and batches them
//! into MTU-sized UDP datagrams via a [`kernel_net::UdpBatcher`].
//!
//! The batching + datagram framing is the shared byte-level core in `kernel-net`
//! ([`kernel_net::UdpBatcher`]); this is the thin `Frame`-encoding wrapper over
//! it. The kernel's telemetry path pushes bytes to the same batcher *already*
//! encoded (its `KernelSink` did the [`protocol::wire_encode`]), so there is one
//! encode step and one datagram format across every consumer. See
//! `docs/network-telemetry-design.md`.

use crate::sink::FrameSink;
use kernel_net::{NetConfig, NetDevice, UdpBatcher};
use protocol::{Frame, wire_encode};

/// Encode scratch, larger than [`kernel_net::MAX_BATCH`] so a frame too large to
/// batch is caught and counted by the batcher rather than silently failing to
/// encode. Real telemetry frames are far smaller (`KernelSink` uses 520).
const ENCODE_SCRATCH: usize = 2048;

/// Encodes each frame and hands the bytes to a [`UdpBatcher`].
pub struct UdpFrameSink<D: NetDevice> {
    batcher: UdpBatcher<D>,
}

impl<D: NetDevice> UdpFrameSink<D> {
    #[must_use]
    pub fn new(config: NetConfig, device: D) -> Self {
        Self { batcher: UdpBatcher::new(config, device) }
    }

    /// Datagrams and frames dropped — see [`UdpBatcher::dropped`].
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.batcher.dropped()
    }

    /// Flush the accumulated batch as one datagram. See [`UdpBatcher::flush`].
    pub fn flush(&mut self) {
        self.batcher.flush();
    }
}

impl<D: NetDevice> FrameSink for UdpFrameSink<D> {
    fn emit(&mut self, frame: &Frame<'_>) {
        let mut scratch = [0u8; ENCODE_SCRATCH];
        if let Ok(bytes) = wire_encode(frame, &mut scratch) {
            self.batcher.push_bytes(bytes);
        }
        // A frame too large to even encode (> ENCODE_SCRATCH) is a programmer
        // error, silently dropped — the same discipline as KernelSink.
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

        assert_eq!(sink.dropped(), 0, "a clean run drops nothing");
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

    #[test]
    fn a_frame_too_big_for_a_datagram_is_dropped_and_counted() {
        let dev = MockNet::new();
        let mut sink = UdpFrameSink::new(test_config(), dev.clone());

        // A single frame larger than a whole batch can't be sent, even alone.
        let huge = "x".repeat(2000);
        sink.emit(&Frame::Log { msg: &huge, task_id: 1, t: 100, hart_id: 0 });
        sink.flush();

        assert_eq!(sink.dropped(), 1, "the un-encodable frame is counted");
        assert!(dev.sent.borrow().is_empty(), "nothing reached the wire");
    }
}
