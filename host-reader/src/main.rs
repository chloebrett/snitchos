//! host-reader: connects to the kernel's telemetry socket, decodes
//! `Frame`s from the byte stream, and pretty-prints each one.
//!
//! v0.1 scope: a single `Frame::Hello` arrives at boot. Later we'll
//! handle continuous span/event/metric streams.

use std::io::Read;
use std::os::unix::net::UnixStream;

use protocol::Frame;

const SOCKET_PATH: &str = "/tmp/snitch-telemetry.sock";

fn main() -> std::io::Result<()> {
    let mut stream = connect()?;
    decode_stream(&mut stream, print_frame)
}

/// Open a connection to the kernel's telemetry socket.
fn connect() -> std::io::Result<UnixStream> {
    UnixStream::connect(SOCKET_PATH)
}

/// Drive the read-decode-emit loop over any byte source. Each fully
/// decoded `Frame` is handed to `on_frame`. Returns when the stream
/// closes cleanly (EOF), or with `Err` on I/O or decode error.
fn decode_stream<R: Read>(
    stream: &mut R,
    mut on_frame: impl FnMut(&Frame<'_>),
) -> std::io::Result<()> {
    let mut buf: Vec<u8> = Vec::with_capacity(1024);
    let mut tmp = [0u8; 256];

    loop {
        // Drain as many complete frames as the buffer currently holds.
        loop {
            let consumed = match try_decode_frame(&buf) {
                Ok((frame, n)) => {
                    on_frame(&frame);
                    n
                }
                Err(postcard::Error::DeserializeUnexpectedEnd) => break, // need more bytes
                Err(e) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("frame decode error: {e:?}"),
                    ));
                }
            };
            buf.drain(..consumed);
        }

        // Read more bytes; EOF returns Ok(0).
        let n = stream.read(&mut tmp)?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&tmp[..n]);
    }
}

/// Pretty-print a decoded frame to stdout. One line per frame.
fn print_frame(frame: &Frame<'_>) {
    match frame {
        Frame::Hello {
            timebase_hz,
            protocol_version,
        } => {
            println!(
                "Hello              timebase={} Hz  protocol_version={}",
                timebase_hz, protocol_version
            );
        }
        Frame::StringRegister { id, value } => {
            println!("StringRegister     id={:?}  value={:?}", id, value);
        }
        Frame::SpanStart {
            id,
            parent,
            name_id,
            t,
        } => {
            println!(
                "SpanStart          id={:?}  parent={:?}  name_id={:?}  t={}",
                id, parent, name_id, t
            );
        }
        Frame::SpanEnd { id, t } => {
            println!("SpanEnd            id={:?}  t={}", id, t);
        }
        Frame::Event {
            span_id,
            name_id,
            t,
        } => {
            println!(
                "Event              span_id={:?}  name_id={:?}  t={}",
                span_id, name_id, t
            );
        }
        Frame::Metric {
            name_id,
            value,
            t,
        } => {
            println!(
                "Metric             name_id={:?}  value={}  t={}",
                name_id, value, t
            );
        }
        Frame::Dropped { count } => {
            println!("Dropped            count={}", count);
        }
    }
}

/// Try to decode one `Frame` from the front of `buf`. Returns the
/// decoded frame and the number of bytes it consumed.
///
/// Returning `Result` (rather than `Option`) keeps the caller honest:
/// `postcard::Error::DeserializeUnexpectedEnd` means "the buffer ended
/// mid-frame, read more"; any other error means "the bytes don't match
/// the protocol," which is worth surfacing rather than silently
/// spinning forever.
fn try_decode_frame<'a>(buf: &'a [u8]) -> Result<(Frame<'a>, usize), postcard::Error> {
    postcard::take_from_bytes(buf)
        .map(|(frame, rest)| (frame, buf.len() - rest.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Round-trip a `Frame::Hello` through encode → try_decode_frame.
    #[test]
    fn decodes_hello() {
        let frame = Frame::Hello {
            timebase_hz: 10_000_000,
            protocol_version: 1,
        };

        let mut buf = [0u8; 64];
        let encoded = postcard::to_slice(&frame, &mut buf).unwrap();
        let encoded_len = encoded.len();

        let (decoded, consumed) = try_decode_frame(encoded).expect("decode should succeed");

        assert_eq!(decoded, frame);
        assert_eq!(consumed, encoded_len);
    }

    /// A truncated buffer (missing the last byte of a frame) should
    /// return `DeserializeUnexpectedEnd` — the "need more bytes"
    /// signal — not some other error variant.
    #[test]
    fn truncated_returns_unexpected_end() {
        let frame = Frame::Hello {
            timebase_hz: 10_000_000,
            protocol_version: 1,
        };

        let mut buf = [0u8; 64];
        let encoded = postcard::to_slice(&frame, &mut buf).unwrap();
        let truncated = &encoded[..encoded.len() - 1];

        let err = try_decode_frame(truncated).expect_err("should fail");
        assert!(
            matches!(err, postcard::Error::DeserializeUnexpectedEnd),
            "expected DeserializeUnexpectedEnd, got {err:?}",
        );
    }

    /// A `Read` impl that hands out at most `chunk_size` bytes per call.
    /// Simulates the real-world behavior of TCP / Unix sockets returning
    /// short reads.
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

    /// Even when the stream returns one byte at a time, `decode_stream`
    /// should accumulate them, decode the complete frame, and emit it.
    #[test]
    fn decode_stream_handles_partial_reads() {
        let frame = Frame::Hello {
            timebase_hz: 10_000_000,
            protocol_version: 1,
        };
        let mut scratch = [0u8; 64];
        let encoded = postcard::to_slice(&frame, &mut scratch).unwrap();

        let reader = ChunkedReader {
            data: encoded.to_vec(),
            pos: 0,
            chunk_size: 1,
        };
        let mut count = 0;
        decode_stream(&mut { reader }, |_| count += 1)
            .expect("decode_stream should succeed");
        assert_eq!(count, 1);
    }

    /// Two encoded frames back-to-back in the stream should both come
    /// out in order.
    #[test]
    fn decode_stream_yields_multiple_frames() {
        use std::io::Cursor;

        let frame_a = Frame::Hello {
            timebase_hz: 10_000_000,
            protocol_version: 1,
        };
        let frame_b = Frame::SpanEnd {
            id: protocol::SpanId(42),
            t: 1234,
        };

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

    /// `decode_stream` should yield exactly the one frame in the input,
    /// then return Ok at EOF.
    #[test]
    fn decode_stream_yields_single_hello() {
        use std::io::Cursor;

        let frame = Frame::Hello {
            timebase_hz: 10_000_000,
            protocol_version: 1,
        };
        let mut buf = [0u8; 64];
        let encoded_len = postcard::to_slice(&frame, &mut buf).unwrap().len();
        let bytes: Vec<u8> = buf[..encoded_len].to_vec();

        let mut count = 0;
        decode_stream(&mut Cursor::new(bytes), |f| {
            assert!(matches!(
                f,
                Frame::Hello {
                    timebase_hz: 10_000_000,
                    protocol_version: 1,
                }
            ));
            count += 1;
        })
        .expect("decode_stream should succeed");

        assert_eq!(count, 1);
    }

    /// Trailing bytes after one frame's encoding must not affect the
    /// `consumed` count. Used to ensure the caller advances exactly past
    /// the frame, ready to decode the next one.
    #[test]
    fn ignores_trailing_bytes() {
        let frame = Frame::Hello {
            timebase_hz: 10_000_000,
            protocol_version: 1,
        };

        let mut buf = [0u8; 64];
        let encoded_len = postcard::to_slice(&frame, &mut buf).unwrap().len();

        // Splice some garbage after the encoded frame.
        let garbage = [0xAAu8, 0xBB, 0xCC, 0xDD];
        let mut combined = Vec::with_capacity(encoded_len + garbage.len());
        combined.extend_from_slice(&buf[..encoded_len]);
        combined.extend_from_slice(&garbage);

        let (decoded, consumed) =
            try_decode_frame(&combined).expect("decode should succeed");

        assert_eq!(decoded, frame);
        assert_eq!(consumed, encoded_len, "consumed must not include trailing bytes");
    }
}
