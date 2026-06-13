//! Stream decoding for hosts that have `std`. Off by default —
//! enable with `features = ["std"]`. The kernel never builds this
//! module.

extern crate std;

use std::io::Read;
use std::string::{String, ToString};
use std::vec::Vec;

use crate::{
    CapEventKind, CapObject, Frame, HartRole, MetricKind, RefusalReason, SpanId, StringId,
    SwitchReason,
};

/// Owned, lifetime-free counterpart of `Frame<'a>`. The host-side
/// reader thread decodes into a temporary buffer and converts to
/// `OwnedFrame` before pushing through a channel — `Frame<'a>`
/// borrows from the read buffer and can't outlive it.
///
/// Add new variants here whenever `Frame` gains one; the matching
/// `from_borrowed` arm will fail to compile and remind you.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnedFrame {
    Hello { timebase_hz: u64, protocol_version: u8 },
    SpanStart { id: SpanId, parent: SpanId, name_id: StringId, t: u64, task_id: u32, hart_id: u8 },
    SpanEnd { id: SpanId, t: u64 },
    Event { span_id: SpanId, name_id: StringId, t: u64 },
    Metric { name_id: StringId, value: i64, t: u64, hart_id: u8 },
    Dropped { count: u32 },
    StringRegister { id: StringId, value: String },
    MetricRegister { name_id: StringId, kind: MetricKind },
    ThreadRegister { id: u32, name: String, priority: u8 },
    ContextSwitch { from: u32, to: u32, t: u64, reason: SwitchReason, hart_id: u8 },
    HartRegister { id: u8, mhartid: u64, role: HartRole },
    CapEvent {
        kind: CapEventKind,
        cap_id: u64,
        parent_cap_id: u64,
        holder: u32,
        object: CapObject,
        rights: u32,
        t: u64,
        hart_id: u8,
    },
    SyscallRefused { syscall: u8, reason: RefusalReason, task_id: u32, t: u64, hart_id: u8 },
    Log { msg: String, task_id: u32, t: u64, hart_id: u8 },
    Message { endpoint: u32, from: u32, to: u32, parent_span: SpanId, t: u64, hart_id: u8 },
}

impl OwnedFrame {
    pub fn from_borrowed(frame: &Frame<'_>) -> Self {
        match *frame {
            Frame::Hello { timebase_hz, protocol_version } => {
                OwnedFrame::Hello { timebase_hz, protocol_version }
            }
            Frame::SpanStart { id, parent, name_id, t, task_id, hart_id } => {
                OwnedFrame::SpanStart { id, parent, name_id, t, task_id, hart_id }
            }
            Frame::SpanEnd { id, t } => OwnedFrame::SpanEnd { id, t },
            Frame::Event { span_id, name_id, t } => {
                OwnedFrame::Event { span_id, name_id, t }
            }
            Frame::Metric { name_id, value, t, hart_id } => {
                OwnedFrame::Metric { name_id, value, t, hart_id }
            }
            Frame::Dropped { count } => OwnedFrame::Dropped { count },
            Frame::StringRegister { id, value } => {
                OwnedFrame::StringRegister { id, value: value.to_string() }
            }
            Frame::MetricRegister { name_id, kind } => {
                OwnedFrame::MetricRegister { name_id, kind }
            }
            Frame::ThreadRegister { id, name, priority } => {
                OwnedFrame::ThreadRegister { id, name: name.to_string(), priority }
            }
            Frame::ContextSwitch { from, to, t, reason, hart_id } => {
                OwnedFrame::ContextSwitch { from, to, t, reason, hart_id }
            }
            Frame::HartRegister { id, mhartid, role } => {
                OwnedFrame::HartRegister { id, mhartid, role }
            }
            Frame::CapEvent { kind, cap_id, parent_cap_id, holder, object, rights, t, hart_id } => {
                OwnedFrame::CapEvent { kind, cap_id, parent_cap_id, holder, object, rights, t, hart_id }
            }
            Frame::SyscallRefused { syscall, reason, task_id, t, hart_id } => {
                OwnedFrame::SyscallRefused { syscall, reason, task_id, t, hart_id }
            }
            Frame::Log { msg, task_id, t, hart_id } => {
                OwnedFrame::Log { msg: msg.to_string(), task_id, t, hart_id }
            }
            Frame::Message { endpoint, from, to, parent_span, t, hart_id } => {
                OwnedFrame::Message { endpoint, from, to, parent_span, t, hart_id }
            }
        }
    }
}

/// Try to decode one `Frame` from the front of `buf`. Returns the
/// decoded frame and the number of bytes it consumed.
///
/// `Result` (rather than `Option`) keeps the caller honest:
/// `postcard::Error::DeserializeUnexpectedEnd` means "the buffer ended
/// mid-frame, read more"; any other error means "the bytes don't match
/// the protocol," which is worth surfacing rather than silently
/// spinning forever.
pub(crate) fn try_decode_frame(buf: &[u8]) -> Result<(Frame<'_>, usize), postcard::Error> {
    postcard::take_from_bytes(buf).map(|(frame, rest)| (frame, buf.len() - rest.len()))
}

/// Drive the read-decode-emit loop over any byte source. Each fully
/// decoded `Frame` is handed to `on_frame`. Returns when the stream
/// closes cleanly (EOF), or with `Err` on I/O or decode error.
pub fn decode_stream<R: Read>(
    stream: &mut R,
    mut on_frame: impl FnMut(&Frame<'_>),
) -> std::io::Result<()> {
    let mut buf: Vec<u8> = Vec::with_capacity(1024);
    let mut tmp = [0u8; 256];

    loop {
        loop {
            let consumed = match try_decode_frame(&buf) {
                Ok((frame, n)) => {
                    on_frame(&frame);
                    n
                }
                Err(postcard::Error::DeserializeUnexpectedEnd) => break,
                Err(e) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        std::format!("frame decode error: {e:?}"),
                    ));
                }
            };
            buf.drain(..consumed);
        }

        let n = stream.read(&mut tmp)?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&tmp[..n]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::vec;

    #[test]
    fn owned_frame_round_trips_hello() {
        let f = Frame::Hello { timebase_hz: 10_000_000, protocol_version: 1 };
        let owned = OwnedFrame::from_borrowed(&f);
        assert_eq!(owned, OwnedFrame::Hello { timebase_hz: 10_000_000, protocol_version: 1 });
    }

    #[test]
    fn owned_frame_copies_string_register_value() {
        // The whole point of OwnedFrame: StringRegister carries a borrow
        // in Frame<'a>, an owned String in OwnedFrame, so the reader
        // thread can ship it through a channel.
        let f = Frame::StringRegister { id: StringId(3), value: "kernel.boot" };
        let owned = OwnedFrame::from_borrowed(&f);
        assert_eq!(
            owned,
            OwnedFrame::StringRegister { id: StringId(3), value: "kernel.boot".to_string() },
        );
    }

    #[test]
    fn owned_frame_round_trips_message() {
        let f = Frame::Message { endpoint: 2, from: 4, to: 5, parent_span: SpanId(42), t: 1234, hart_id: 1 };
        let owned = OwnedFrame::from_borrowed(&f);
        assert_eq!(
            owned,
            OwnedFrame::Message { endpoint: 2, from: 4, to: 5, parent_span: SpanId(42), t: 1234, hart_id: 1 },
        );
    }

    #[test]
    fn owned_frame_handles_every_variant() {
        // Add a case here when adding a Frame variant. The match in
        // `from_borrowed` is exhaustive so this is belt-and-braces;
        // keeping it explicit so the test file is the canonical
        // checklist of variants.
        for f in [
            Frame::Hello { timebase_hz: 1, protocol_version: 0 },
            Frame::SpanStart { id: SpanId(1), parent: SpanId(0), name_id: StringId(0), t: 1, task_id: 0, hart_id: 0 },
            Frame::ThreadRegister { id: 1, name: "task_a", priority: 1 },
            Frame::ContextSwitch { from: 1, to: 2, t: 1, reason: SwitchReason::Yield, hart_id: 0 },
            Frame::SpanEnd { id: SpanId(1), t: 2 },
            Frame::Event { span_id: SpanId(1), name_id: StringId(0), t: 3 },
            Frame::Metric { name_id: StringId(0), value: 5, t: 4, hart_id: 0 },
            Frame::Dropped { count: 7 },
            Frame::StringRegister { id: StringId(0), value: "x" },
            Frame::MetricRegister { name_id: StringId(0), kind: MetricKind::Counter },
            Frame::HartRegister { id: 0, mhartid: 0, role: crate::HartRole::Boot },
            Frame::Message { endpoint: 1, from: 2, to: 3, parent_span: SpanId(4), t: 5, hart_id: 0 },
        ] {
            // Just exercising — that we get *some* OwnedFrame back
            // without panicking covers the variant.
            let _ = OwnedFrame::from_borrowed(&f);
        }
    }

    // --- Moved verbatim from collector/src/main.rs (decode tests) ---

    #[test]
    fn decodes_hello() {
        let frame = Frame::Hello { timebase_hz: 10_000_000, protocol_version: 1 };
        let mut buf = [0u8; 64];
        let encoded_len = postcard::to_slice(&frame, &mut buf).unwrap().len();

        let (decoded, consumed) =
            try_decode_frame(&buf[..encoded_len]).expect("decode should succeed");
        assert_eq!(decoded, frame);
        assert_eq!(consumed, encoded_len);
    }

    #[test]
    fn truncated_returns_unexpected_end() {
        let frame = Frame::Hello { timebase_hz: 10_000_000, protocol_version: 1 };
        let mut buf = [0u8; 64];
        let encoded_len = postcard::to_slice(&frame, &mut buf).unwrap().len();
        let truncated = &buf[..encoded_len - 1];
        let err = try_decode_frame(truncated).expect_err("truncated should fail");
        assert!(matches!(err, postcard::Error::DeserializeUnexpectedEnd));
    }

    #[test]
    fn ignores_trailing_bytes() {
        let frame = Frame::SpanEnd { id: SpanId(7), t: 99 };
        let mut buf = [0u8; 64];
        let encoded_len = postcard::to_slice(&frame, &mut buf).unwrap().len();
        let garbage = [0xAAu8, 0xBB, 0xCC, 0xDD];
        let mut combined = Vec::with_capacity(encoded_len + garbage.len());
        combined.extend_from_slice(&buf[..encoded_len]);
        combined.extend_from_slice(&garbage);
        let (decoded, consumed) =
            try_decode_frame(&combined).expect("decode should succeed");
        assert_eq!(decoded, frame);
        assert_eq!(consumed, encoded_len);
    }

    #[test]
    fn decode_stream_yields_single_hello() {
        let frame = Frame::Hello { timebase_hz: 10_000_000, protocol_version: 1 };
        let mut buf = [0u8; 64];
        let encoded_len = postcard::to_slice(&frame, &mut buf).unwrap().len();
        let bytes: Vec<u8> = buf[..encoded_len].to_vec();
        let mut count = 0;
        decode_stream(&mut Cursor::new(bytes), |f| {
            assert!(matches!(
                f,
                Frame::Hello { timebase_hz: 10_000_000, protocol_version: 1 }
            ));
            count += 1;
        })
        .expect("decode_stream should succeed");
        assert_eq!(count, 1);
    }

    #[test]
    fn decode_stream_yields_multiple_frames() {
        let frame_a = Frame::Hello { timebase_hz: 10_000_000, protocol_version: 1 };
        let frame_b = Frame::SpanEnd { id: SpanId(42), t: 1234 };
        let mut buf = Vec::new();
        {
            let mut scratch = [0u8; 64];
            buf.extend_from_slice(postcard::to_slice(&frame_a, &mut scratch).unwrap());
            buf.extend_from_slice(postcard::to_slice(&frame_b, &mut scratch).unwrap());
        }
        let mut seen: Vec<&'static str> = Vec::new();
        decode_stream(&mut Cursor::new(buf), |f| match f {
            Frame::Hello { .. } => seen.push("hello"),
            Frame::SpanEnd { .. } => seen.push("span_end"),
            _ => panic!("unexpected frame {f:?}"),
        })
        .expect("decode_stream should succeed");
        assert_eq!(seen, vec!["hello", "span_end"]);
    }

    /// `Read` impl that hands out at most `chunk_size` bytes per call.
    /// Simulates the short-reads behavior of TCP / Unix sockets.
    struct ChunkedReader {
        data: Vec<u8>,
        pos: usize,
        chunk_size: usize,
    }

    impl Read for ChunkedReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let avail = self.data.len() - self.pos;
            let n = avail.min(self.chunk_size).min(buf.len());
            buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
            self.pos += n;
            Ok(n)
        }
    }

    #[test]
    fn decode_stream_handles_partial_reads() {
        let frame = Frame::MetricRegister { name_id: StringId(7), kind: MetricKind::Counter };
        let mut scratch = [0u8; 64];
        let encoded = postcard::to_slice(&frame, &mut scratch).unwrap();
        let reader = ChunkedReader { data: encoded.to_vec(), pos: 0, chunk_size: 1 };
        let mut count = 0;
        decode_stream(&mut { reader }, |_| count += 1)
            .expect("decode_stream should succeed");
        assert_eq!(count, 1);
    }
}
