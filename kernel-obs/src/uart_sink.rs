//! `UartFrameSink` — a [`FrameSink`] that encodes each frame and pushes the
//! COBS-framed bytes into a byte sink (the kernel's THRE-drained TX ring), the
//! serial mirror of [`crate::udp_sink::UdpFrameSink`].
//!
//! Same one-encode discipline as every other sink: [`protocol::wire_encode`]
//! produces the `0x00`-delimited bytes the host collector decodes. Backpressure
//! is **whole-frame atomic** — a frame that won't wholly fit the sink is dropped
//! and counted (never split, so the COBS stream stays clean), surfaced as
//! `Frame::Dropped` by the heartbeat. Never blocks: the kernel must not stall on
//! the wire.

use crate::sink::FrameSink;
use protocol::{Frame, wire_encode};

/// Encode scratch. Matches the virtio `KernelSink`'s 520-byte buffer, so any
/// frame that path can encode this one can too.
const ENCODE_SCRATCH: usize = 520;

/// A byte destination with all-or-nothing writes. The kernel backs this with the
/// TX ring; tests back it with a mock.
pub trait ByteSink {
    /// Push every byte of `bytes`, or none. Returns `false` (writing nothing) if
    /// they don't all fit — the caller counts one whole-frame drop.
    fn push_all(&mut self, bytes: &[u8]) -> bool;
}

/// Encodes each frame and hands the whole encoded frame to a [`ByteSink`].
pub struct UartFrameSink<S: ByteSink> {
    sink: S,
    dropped: u32,
}

impl<S: ByteSink> UartFrameSink<S> {
    pub fn new(sink: S) -> Self {
        Self { sink, dropped: 0 }
    }

    /// Frames dropped for want of ring space (or too large to encode). Drained by
    /// the heartbeat as `Frame::Dropped`.
    #[must_use]
    pub fn dropped(&self) -> u32 {
        self.dropped
    }
}

impl<S: ByteSink> FrameSink for UartFrameSink<S> {
    fn emit(&mut self, frame: &Frame<'_>) {
        let mut scratch = [0u8; ENCODE_SCRATCH];
        // A frame too large to encode (> ENCODE_SCRATCH) counts as a drop, the
        // same discipline as the virtio and UDP sinks.
        let Ok(bytes) = wire_encode(frame, &mut scratch) else {
            self.dropped += 1;
            return;
        };
        if !self.sink.push_all(bytes) {
            self.dropped += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::rc::Rc;
    use alloc::vec::Vec;
    use core::cell::{Cell, RefCell};
    use protocol::SpanId;

    /// A `ByteSink` that records whole frames it accepts and can be told to reject
    /// (as if the ring were full). Behind shared handles so the test inspects /
    /// toggles it after the sink has taken it by value.
    #[derive(Clone)]
    struct MockRing {
        written: Rc<RefCell<Vec<Vec<u8>>>>,
        full: Rc<Cell<bool>>,
    }

    impl MockRing {
        fn new() -> Self {
            Self { written: Rc::new(RefCell::new(Vec::new())), full: Rc::new(Cell::new(false)) }
        }
    }

    impl ByteSink for MockRing {
        fn push_all(&mut self, bytes: &[u8]) -> bool {
            if self.full.get() {
                return false;
            }
            self.written.borrow_mut().push(bytes.to_vec());
            true
        }
    }

    #[test]
    fn a_frame_encodes_and_arrives_whole() {
        let ring = MockRing::new();
        let mut sink = UartFrameSink::new(ring.clone());

        let frame = Frame::SpanEnd { id: SpanId(7), t: 42 };
        sink.emit(&frame);

        assert_eq!(sink.dropped(), 0, "a frame that fits drops nothing");
        let written = ring.written.borrow();
        assert_eq!(written.len(), 1, "the whole frame reached the ring");
        let mut buf = written[0].clone();
        let got: Frame = postcard::from_bytes_cobs(&mut buf).expect("decodes");
        assert_eq!(got, frame);
    }

    #[test]
    fn a_frame_that_wont_fit_is_dropped_whole_and_counted() {
        let ring = MockRing::new();
        let mut sink = UartFrameSink::new(ring.clone());

        ring.full.set(true);
        sink.emit(&Frame::SpanEnd { id: SpanId(1), t: 10 });

        assert_eq!(sink.dropped(), 1, "the refused frame is counted");
        assert!(ring.written.borrow().is_empty(), "nothing partial reached the ring");
    }

    #[test]
    fn a_frame_too_big_to_encode_is_dropped_and_counted() {
        let ring = MockRing::new();
        let mut sink = UartFrameSink::new(ring.clone());

        // A Log message past the 520-byte scratch can't be encoded at all.
        let huge = "x".repeat(600);
        sink.emit(&Frame::Log { msg: &huge, task_id: 1, t: 100, hart_id: 0 });

        assert_eq!(sink.dropped(), 1, "the un-encodable frame is counted");
        assert!(ring.written.borrow().is_empty(), "nothing reached the ring");
    }

    #[test]
    fn the_sink_recovers_after_a_drop() {
        let ring = MockRing::new();
        let mut sink = UartFrameSink::new(ring.clone());

        ring.full.set(true);
        sink.emit(&Frame::SpanEnd { id: SpanId(1), t: 10 });
        assert_eq!(sink.dropped(), 1, "first frame dropped");

        ring.full.set(false);
        let frame = Frame::SpanEnd { id: SpanId(2), t: 20 };
        sink.emit(&frame);

        assert_eq!(sink.dropped(), 1, "a frame that fits doesn't count a drop");
        let written = ring.written.borrow();
        assert_eq!(written.len(), 1, "the sink recovers after a drop");
        let mut buf = written[0].clone();
        let got: Frame = postcard::from_bytes_cobs(&mut buf).expect("decodes");
        assert_eq!(got, frame);
    }
}
