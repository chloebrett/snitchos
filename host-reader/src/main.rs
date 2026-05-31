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
    let stream = connect()?;
    read_frames(stream)?;
    Ok(())
}

/// Open a connection to the kernel's telemetry socket.
fn connect() -> std::io::Result<UnixStream> {
    todo!()
}

/// Read frames from the stream until it closes or errors. Decode each
/// frame and hand it off to `print_frame`.
fn read_frames(stream: UnixStream) -> std::io::Result<()> {
    todo!()
}

/// Pretty-print a decoded frame to stdout.
fn print_frame(frame: &Frame<'_>) {
    todo!()
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
