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
    MetricRegister { name_id: StringId, kind: MetricKind, task_id: u32 },
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
        badge: u64,
        t: u64,
        hart_id: u8,
        /// The object's name, NUL-padded (see [`snitchos_abi::name_str`]).
        name: [u8; snitchos_abi::CAP_NAME_LEN],
    },
    SyscallRefused { syscall: u8, reason: RefusalReason, task_id: u32, t: u64, hart_id: u8 },
    Log { msg: String, task_id: u32, t: u64, hart_id: u8 },
    Message { endpoint: u32, from: u32, to: u32, parent_span: SpanId, t: u64, hart_id: u8 },
    NotifySignal { notification: u32, mask: u64, from_task: u32, t: u64, hart_id: u8 },
    NotifyWait { notification: u32, bits: u64, to_task: u32, t: u64, hart_id: u8 },
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
            Frame::MetricRegister { name_id, kind, task_id } => {
                OwnedFrame::MetricRegister { name_id, kind, task_id }
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
            Frame::CapEvent { kind, cap_id, parent_cap_id, holder, object, rights, badge, t, hart_id, name } => {
                OwnedFrame::CapEvent { kind, cap_id, parent_cap_id, holder, object, rights, badge, t, hart_id, name }
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
            Frame::NotifySignal { notification, mask, from_task, t, hart_id } => {
                OwnedFrame::NotifySignal { notification, mask, from_task, t, hart_id }
            }
            Frame::NotifyWait { notification, bits, to_task, t, hart_id } => {
                OwnedFrame::NotifyWait { notification, bits, to_task, t, hart_id }
            }
        }
    }
}

/// Try to decode one COBS-framed [`Frame`] (see [`crate::wire_encode`]) from the front of
/// `buf`, returning it in owned form plus the number of bytes it consumed (the
/// frame body **and** its `0x00` terminator).
///
/// `Ok(None)` means "no complete frame yet — the buffer holds no `0x00`
/// terminator, read more." `Err` means a `0x00`-delimited chunk was present but
/// failed to COBS/postcard-decode (corruption): the caller drops it and continues
/// from the byte after the terminator (`n` still points past the delimiter), which
/// is the resync — the delimiter is where the stream finds its feet again.
///
/// Returns [`OwnedFrame`] rather than a borrowed [`Frame`]: COBS decodes in place,
/// and the decode happens in a scratch buffer here, so nothing borrowed can
/// escape. Every existing caller converts to [`OwnedFrame`] immediately anyway.
///
/// Public so a caller holding a *growing in-memory* buffer (rather than a `Read`
/// stream) can decode incrementally — advancing an offset by the returned count.
pub fn try_decode_frame(buf: &[u8]) -> Result<Option<(OwnedFrame, usize)>, DecodeError> {
    let Some(zero) = buf.iter().position(|&b| b == 0) else {
        return Ok(None); // no terminator yet — need more bytes
    };
    let consumed = zero + 1;
    let mut chunk = buf[..consumed].to_vec();
    match postcard::take_from_bytes_cobs::<Frame<'_>>(&mut chunk) {
        Ok((frame, _)) => Ok(Some((OwnedFrame::from_borrowed(&frame), consumed))),
        Err(_) => Err(DecodeError { consumed }),
    }
}

/// A `0x00`-delimited chunk failed to decode. Carries how many bytes to skip
/// (through the terminator) so the caller can resync at the next frame boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeError {
    /// Bytes to advance past the bad frame (including its `0x00` terminator).
    pub consumed: usize,
}

/// How [`decode_stream`] reacts to a `0x00`-delimited chunk that fails to decode.
/// The right choice is a property of the *transport*, not the payload — hence a
/// type at the call site, not a bool buried in the loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnDecodeError {
    /// Return `Err`. A lossless transport (a Unix socket, an in-memory buffer)
    /// never drops bytes, so an undecodable frame is a real bug worth surfacing —
    /// not noise to swallow.
    Fail,
    /// Skip the bad chunk — resync at the next `0x00` — and keep going, counting
    /// it in the returned [`DecodeSummary`]. A lossy transport (a serial line)
    /// drops bytes; one corrupt frame must not kill the stream. Resync is *free*
    /// with COBS framing: the delimiter is already where the next frame begins.
    Resync,
}

/// What a [`decode_stream`] run observed by the time the source closed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DecodeSummary {
    /// Frames skipped because they failed to decode — only ever non-zero under
    /// [`OnDecodeError::Resync`]. This is telemetry about the *transport* (how
    /// lossy the wire is), worth exporting in its own right.
    pub resyncs: u64,
}

/// Drive the read-decode-emit loop over any byte source. Each COBS-framed
/// [`Frame`] (see [`crate::wire_encode`]) is handed to `on_frame` in borrowed form.
/// Returns the run's [`DecodeSummary`] on clean EOF; under [`OnDecodeError::Fail`]
/// returns `Err` on the first undecodable chunk.
pub fn decode_stream<R: Read>(
    stream: &mut R,
    on_error: OnDecodeError,
    mut on_frame: impl FnMut(&Frame<'_>),
) -> std::io::Result<DecodeSummary> {
    let mut buf: Vec<u8> = Vec::with_capacity(1024);
    let mut tmp = [0u8; 256];
    let mut summary = DecodeSummary::default();

    loop {
        // Decode every complete (`0x00`-terminated) frame in the buffer. COBS
        // decode is in place, so we drain a prefix into a scratch Vec and hand the
        // borrow to `on_frame` before dropping it.
        while let Some(zero) = buf.iter().position(|&b| b == 0) {
            let mut chunk: Vec<u8> = buf.drain(..=zero).collect();
            match postcard::take_from_bytes_cobs::<Frame<'_>>(&mut chunk) {
                Ok((frame, _)) => on_frame(&frame),
                Err(e) => match on_error {
                    // Dropping `chunk` past its delimiter *is* the resync.
                    OnDecodeError::Resync => summary.resyncs += 1,
                    OnDecodeError::Fail => {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            std::format!("frame decode error: {e:?}"),
                        ));
                    }
                },
            }
        }

        let n = stream.read(&mut tmp)?;
        if n == 0 {
            return Ok(summary);
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
    fn owned_frame_round_trips_cap_event_with_badge() {
        // The badge must survive Frame -> OwnedFrame, or the collector/harness
        // see a zeroed demux value. A field copy mutation testing can't reach.
        let f = Frame::CapEvent {
            kind: CapEventKind::Transferred,
            cap_id: 9,
            parent_cap_id: 2,
            holder: 5,
            object: CapObject::Endpoint,
            rights: 0b0010,
            badge: 0xCAFE,
            t: 7777,
            hart_id: 1,
            name: snitchos_abi::pack_name("fs"),
        };
        let owned = OwnedFrame::from_borrowed(&f);
        assert_eq!(
            owned,
            OwnedFrame::CapEvent {
                kind: CapEventKind::Transferred,
                cap_id: 9,
                parent_cap_id: 2,
                holder: 5,
                object: CapObject::Endpoint,
                rights: 0b0010,
                badge: 0xCAFE,
                t: 7777,
                hart_id: 1,
                name: snitchos_abi::pack_name("fs"),
            },
        );
        // The object name must survive too, or the host loses the named tree.
        let OwnedFrame::CapEvent { name, .. } = owned else { panic!("a CapEvent") };
        assert_eq!(snitchos_abi::name_str(&name), "fs");
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
            Frame::MetricRegister { name_id: StringId(0), kind: MetricKind::Counter, task_id: 0 },
            Frame::HartRegister { id: 0, mhartid: 0, role: crate::HartRole::Boot },
            Frame::Message { endpoint: 1, from: 2, to: 3, parent_span: SpanId(4), t: 5, hart_id: 0 },
            Frame::CapEvent { kind: CapEventKind::Granted, cap_id: 1, parent_cap_id: 0, holder: 1, object: CapObject::Endpoint, rights: 0b0010, badge: 0, t: 1, hart_id: 0, name: [0; snitchos_abi::CAP_NAME_LEN] },
        ] {
            // Just exercising — that we get *some* OwnedFrame back
            // without panicking covers the variant.
            let _ = OwnedFrame::from_borrowed(&f);
        }
    }

    // --- COBS wire framing (Step 1: docs/uart-telemetry-design.md Decision 1) ---

    #[test]
    fn wire_encoded_frame_round_trips_through_the_stream_decoder() {
        let frame = Frame::Hello { timebase_hz: 10_000_000, protocol_version: 1 };
        let mut buf = [0u8; 64];
        let bytes = crate::wire_encode(&frame, &mut buf).expect("encode fits").to_vec();
        let mut got = None;
        decode_stream(&mut Cursor::new(bytes), OnDecodeError::Fail, |f| got = Some(OwnedFrame::from_borrowed(f)))
            .expect("clean stream decodes");
        assert_eq!(got, Some(OwnedFrame::from_borrowed(&frame)));
    }

    #[test]
    fn a_frame_whose_encoding_contains_a_zero_byte_survives_the_delimiter() {
        // The whole reason for COBS: `0x00` is the frame delimiter, so any `0x00`
        // *inside* an encoded frame would be a false boundary. A field of value 0
        // (t = 0) postcard-encodes to a literal `0x00`; this frame must still
        // round-trip whole, proving the delimiter can't occur in the payload.
        let frame = Frame::SpanEnd { id: SpanId(0), t: 0 };
        let mut buf = [0u8; 64];
        let bytes = crate::wire_encode(&frame, &mut buf).expect("encode fits");
        assert!(
            bytes[..bytes.len() - 1].iter().all(|&b| b != 0),
            "COBS output before the trailing delimiter must contain no 0x00"
        );
        let mut got = None;
        decode_stream(&mut Cursor::new(bytes.to_vec()), OnDecodeError::Fail, |f| got = Some(OwnedFrame::from_borrowed(f)))
            .expect("clean stream decodes");
        assert_eq!(got, Some(OwnedFrame::from_borrowed(&frame)));
    }

    #[test]
    fn two_wire_frames_back_to_back_both_decode() {
        let a = Frame::Hello { timebase_hz: 10_000_000, protocol_version: 1 };
        let b = Frame::SpanEnd { id: SpanId(42), t: 1234 };
        let mut wire = Vec::new();
        let mut buf = [0u8; 64];
        wire.extend_from_slice(crate::wire_encode(&a, &mut buf).unwrap());
        wire.extend_from_slice(crate::wire_encode(&b, &mut buf).unwrap());
        let mut got = Vec::new();
        decode_stream(&mut Cursor::new(wire), OnDecodeError::Fail, |f| got.push(OwnedFrame::from_borrowed(f)))
            .expect("clean stream decodes");
        assert_eq!(got, vec![OwnedFrame::from_borrowed(&a), OwnedFrame::from_borrowed(&b)]);
    }

    // --- Decode tests (COBS-framed; see `wire_encode`) ---

    /// Encode `frame` through the wire framing into an owned `Vec` — the tests'
    /// stand-in for what the kernel puts on the wire.
    fn wire(frame: &Frame<'_>) -> Vec<u8> {
        let mut buf = [0u8; 128];
        crate::wire_encode(frame, &mut buf).expect("encode fits").to_vec()
    }

    /// A `0x00`-delimited chunk whose body can't decode — an empty COBS frame
    /// (`0x01` = zero data bytes), which postcard rejects. Stands in for a frame a
    /// lossy serial line corrupted while leaving the delimiters intact.
    const BAD_CHUNK: [u8; 2] = [0x01, 0x00];

    #[test]
    fn resync_skips_a_corrupt_frame_and_delivers_its_neighbours() {
        let a = Frame::Hello { timebase_hz: 10_000_000, protocol_version: 1 };
        let c = Frame::SpanEnd { id: SpanId(9), t: 5 };
        let mut stream = wire(&a);
        stream.extend_from_slice(&BAD_CHUNK); // corruption between two good frames
        stream.extend_from_slice(&wire(&c));
        let mut got = Vec::new();
        let summary = decode_stream(&mut Cursor::new(stream), OnDecodeError::Resync, |f| {
            got.push(OwnedFrame::from_borrowed(f));
        })
        .expect("resync never fails");
        assert_eq!(got, vec![OwnedFrame::from_borrowed(&a), OwnedFrame::from_borrowed(&c)]);
        assert_eq!(summary.resyncs, 1, "the one bad frame was counted");
    }

    #[test]
    fn fail_policy_returns_err_on_a_corrupt_frame() {
        // Same input, lossless policy: an undecodable frame is a real bug, surfaced.
        let mut stream = wire(&Frame::Hello { timebase_hz: 1, protocol_version: 1 });
        stream.extend_from_slice(&BAD_CHUNK);
        let err = decode_stream(&mut Cursor::new(stream), OnDecodeError::Fail, |_| {})
            .expect_err("fail policy surfaces the bad frame");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn resync_on_a_clean_stream_reports_zero_resyncs() {
        let a = Frame::Hello { timebase_hz: 1, protocol_version: 1 };
        let b = Frame::SpanEnd { id: SpanId(1), t: 2 };
        let mut stream = wire(&a);
        stream.extend_from_slice(&wire(&b));
        let mut count = 0;
        let summary = decode_stream(&mut Cursor::new(stream), OnDecodeError::Resync, |_| count += 1)
            .expect("clean stream decodes");
        assert_eq!(count, 2);
        assert_eq!(summary.resyncs, 0, "a clean stream never resyncs");
    }

    #[test]
    fn resync_recovers_from_consecutive_corrupt_frames() {
        // Two bad chunks in a row must not wedge the resync loop.
        let good = Frame::SpanEnd { id: SpanId(7), t: 3 };
        let mut stream = Vec::new();
        stream.extend_from_slice(&BAD_CHUNK);
        stream.extend_from_slice(&BAD_CHUNK);
        stream.extend_from_slice(&wire(&good));
        let mut got = Vec::new();
        let summary = decode_stream(&mut Cursor::new(stream), OnDecodeError::Resync, |f| {
            got.push(OwnedFrame::from_borrowed(f));
        })
        .expect("resync never fails");
        assert_eq!(got, vec![OwnedFrame::from_borrowed(&good)]);
        assert_eq!(summary.resyncs, 2);
    }

    #[test]
    fn decodes_hello() {
        let frame = Frame::Hello { timebase_hz: 10_000_000, protocol_version: 1 };
        let bytes = wire(&frame);
        let (decoded, consumed) =
            try_decode_frame(&bytes).expect("no decode error").expect("a complete frame");
        assert_eq!(decoded, OwnedFrame::from_borrowed(&frame));
        assert_eq!(consumed, bytes.len());
    }

    #[test]
    fn round_trips_a_revoked_cap_event() {
        // The `Revoked` kind is the newest CapEventKind discriminant (appended
        // after Granted/Transferred). Round-trip it so a future reorder — which
        // would silently break the wire — fails this test.
        let frame = Frame::CapEvent {
            kind: CapEventKind::Revoked,
            cap_id: 77,
            parent_cap_id: 3,
            holder: 4,
            object: CapObject::Endpoint,
            rights: 0b0010,
            badge: 0,
            t: 123,
            hart_id: 0,
            name: [0; snitchos_abi::CAP_NAME_LEN],
        };
        let (decoded, _) =
            try_decode_frame(&wire(&frame)).expect("no decode error").expect("a frame");
        assert!(matches!(
            decoded,
            OwnedFrame::CapEvent { kind: CapEventKind::Revoked, cap_id: 77, holder: 4, .. },
        ));
    }

    #[test]
    fn round_trips_a_minted_cap_event() {
        // `Minted` is the newest CapEventKind discriminant (appended after
        // Revoked — self-minted-via-syscall provenance). Round-trip guards a
        // future reorder.
        let frame = Frame::CapEvent {
            kind: CapEventKind::Minted,
            cap_id: 88,
            parent_cap_id: 0,
            holder: 5,
            object: CapObject::Endpoint,
            rights: 0b0110,
            badge: 0,
            t: 456,
            hart_id: 1,
            name: [0; snitchos_abi::CAP_NAME_LEN],
        };
        let (decoded, _) =
            try_decode_frame(&wire(&frame)).expect("no decode error").expect("a frame");
        assert!(matches!(
            decoded,
            OwnedFrame::CapEvent { kind: CapEventKind::Minted, cap_id: 88, holder: 5, .. },
        ));
    }

    #[test]
    fn a_buffer_without_a_delimiter_yields_no_frame_yet() {
        // Truncation before the `0x00` terminator (the COBS body has no interior
        // zeros by construction) means "read more", not an error.
        let bytes = wire(&Frame::Hello { timebase_hz: 10_000_000, protocol_version: 1 });
        let truncated = &bytes[..bytes.len() - 1]; // drops the terminator
        assert_eq!(try_decode_frame(truncated), Ok(None));
    }

    #[test]
    fn a_chunk_that_fails_to_decode_reports_an_error_not_a_panic() {
        // `[0x01, 0x00]` is a well-formed COBS frame whose decoded body is empty;
        // postcard can't read a `Frame` from zero bytes. `try_decode_frame` reports
        // the error with a resync offset past the delimiter — Step 2 uses that
        // offset to skip and continue rather than dying.
        assert_eq!(try_decode_frame(&[0x01, 0x00]), Err(DecodeError { consumed: 2 }));
    }

    #[test]
    fn try_decode_frame_consumes_exactly_through_the_first_delimiter() {
        // Bytes after a frame's terminator belong to the next frame, not this one.
        let a = Frame::SpanEnd { id: SpanId(7), t: 99 };
        let b = Frame::Hello { timebase_hz: 1, protocol_version: 1 };
        let a_bytes = wire(&a);
        let mut combined = a_bytes.clone();
        combined.extend_from_slice(&wire(&b));
        let (decoded, consumed) =
            try_decode_frame(&combined).expect("no decode error").expect("first frame");
        assert_eq!(decoded, OwnedFrame::from_borrowed(&a));
        assert_eq!(consumed, a_bytes.len(), "stops at the first delimiter");
    }

    #[test]
    fn decode_stream_yields_single_hello() {
        let frame = Frame::Hello { timebase_hz: 10_000_000, protocol_version: 1 };
        let mut count = 0;
        decode_stream(&mut Cursor::new(wire(&frame)), OnDecodeError::Fail, |f| {
            assert!(matches!(f, Frame::Hello { timebase_hz: 10_000_000, protocol_version: 1 }));
            count += 1;
        })
        .expect("decode_stream should succeed");
        assert_eq!(count, 1);
    }

    #[test]
    fn decode_stream_yields_multiple_frames() {
        let frame_a = Frame::Hello { timebase_hz: 10_000_000, protocol_version: 1 };
        let frame_b = Frame::SpanEnd { id: SpanId(42), t: 1234 };
        let mut buf = wire(&frame_a);
        buf.extend_from_slice(&wire(&frame_b));
        let mut seen: Vec<&'static str> = Vec::new();
        decode_stream(&mut Cursor::new(buf), OnDecodeError::Fail, |f| match f {
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
        let frame = Frame::MetricRegister { name_id: StringId(7), kind: MetricKind::Counter, task_id: 9 };
        let reader = ChunkedReader { data: wire(&frame), pos: 0, chunk_size: 1 };
        let mut count = 0;
        decode_stream(&mut { reader }, OnDecodeError::Fail, |_| count += 1)
            .expect("decode_stream should succeed");
        assert_eq!(count, 1);
    }
}
