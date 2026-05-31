//! collector: connects to the kernel's telemetry socket, decodes
//! `Frame`s from the byte stream, and routes them to one or more
//! output sinks (stdout / OTLP / Prometheus).
//!
//! v0.2 scope: `--text` works; `--otlp` and `--prometheus` are stubs
//! that print "not yet implemented" — wired up in later steps.

use std::io::Read;
use std::os::unix::net::UnixStream;

use clap::Parser;
use protocol::Frame;

const SOCKET_PATH: &str = "/tmp/snitch-telemetry.sock";

/// Connect to the kernel's telemetry socket, decode `Frame`s, and route
/// them to the configured output sinks. At least one of `--text`,
/// `--otlp`, or `--prometheus` must be enabled.
#[derive(Parser)]
#[command(about, version)]
struct Args {
    /// Print decoded frames to stdout.
    #[arg(long)]
    text: bool,

    /// Use multi-line Debug format when `--text` is enabled.
    #[arg(long)]
    pretty: bool,

    /// OTLP/HTTP endpoint for trace export (e.g.
    /// `http://localhost:4318`). NOT YET IMPLEMENTED.
    #[arg(long)]
    otlp: Option<String>,

    /// TCP port to serve Prometheus `/metrics` on. NOT YET IMPLEMENTED.
    #[arg(long)]
    prometheus: Option<u16>,
}

fn main() -> std::io::Result<()> {
    let args = Args::parse();

    if !args.text && args.otlp.is_none() && args.prometheus.is_none() {
        eprintln!(
            "error: must specify at least one output: --text, --otlp <url>, or --prometheus <port>",
        );
        std::process::exit(2);
    }

    if let Some(endpoint) = &args.otlp {
        eprintln!("warning: --otlp {endpoint} not yet implemented; ignoring");
    }
    if let Some(port) = args.prometheus {
        eprintln!("warning: --prometheus {port} not yet implemented; ignoring");
    }

    let mut stream = UnixStream::connect(SOCKET_PATH)?;
    decode_stream(&mut stream, |frame| {
        if args.text {
            print_frame(frame, args.pretty);
        }
    })?;

    eprintln!("kernel disconnected; restart with `cargo xtask collect`");
    Ok(())
}

/// Try to decode one `Frame` from the front of `buf`. Returns the
/// decoded frame and the number of bytes it consumed.
///
/// `Result` (rather than `Option`) keeps the caller honest:
/// `postcard::Error::DeserializeUnexpectedEnd` means "the buffer ended
/// mid-frame, read more"; any other error means "the bytes don't match
/// the protocol," which is worth surfacing rather than silently
/// spinning forever.
fn try_decode_frame<'a>(buf: &'a [u8]) -> Result<(Frame<'a>, usize), postcard::Error> {
    postcard::take_from_bytes(buf).map(|(frame, rest)| (frame, buf.len() - rest.len()))
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
                Err(postcard::Error::DeserializeUnexpectedEnd) => break,
                Err(e) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("frame decode error: {e:?}"),
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

/// Print a decoded frame to stdout. Uses the derived `Debug` impl; with
/// `pretty=true`, multi-line pretty format for easier inspection.
fn print_frame(frame: &Frame<'_>, pretty: bool) {
    if pretty {
        println!("{frame:#?}");
    } else {
        println!("{frame:?}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::{MetricKind, SpanId, StringId};
    use std::io::Cursor;

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

    #[test]
    fn ignores_trailing_bytes() {
        let frame = Frame::Hello {
            timebase_hz: 10_000_000,
            protocol_version: 1,
        };
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

    #[test]
    fn decode_stream_yields_multiple_frames() {
        let frame_a = Frame::Hello {
            timebase_hz: 10_000_000,
            protocol_version: 1,
        };
        let frame_b = Frame::SpanEnd {
            id: SpanId(42),
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
        let frame = Frame::MetricRegister {
            name_id: StringId(7),
            kind: MetricKind::Counter,
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
}
