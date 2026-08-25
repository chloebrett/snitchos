//! Serial-line logic shared by every consumer of the board's UART.
//!
//! Deliberately **not** behind the `native` feature, though it looks like host
//! I/O: nothing here opens a port. `call_out_alternative` is string manipulation
//! and [`SerialReader`] is generic over any [`Read`] — the one genuinely native
//! act, `serialport::new(…).open()`, stays in `crate::source`.
//!
//! (That reference is deliberately *not* an intra-doc link: `crate::source` is
//! `native`-gated, so linking it would fail to resolve under
//! `--no-default-features` — the exact configuration this module exists to
//! serve.)
//!
//! That split is what lets a second consumer reuse this. `cargo xtask board`
//! (see `plans/board-bridge.md`) needs exactly these two behaviours and must not
//! inherit the collector's HTTP stack to get them: the `native` feature pulls
//! `ureq` → `ring` and `tiny_http`, and the tool that runs `cargo xtask test`
//! has no business compiling a web server. Same reasoning as the `xtask-itest`
//! split — keep the heavy dependency out of the lean path.

use std::io::Read;

/// The serial port's read timeout — what paces [`SerialReader`]'s idle loop.
///
/// It is not a deadline on the board: an idle port must not busy-wait, and a
/// responsive Ctrl-C wants the loop to come up for air regularly. This value is
/// the only thing standing between "quiet board" and "pinned CPU core".
pub const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(200);

/// The `cu.*` path to use instead, if `device` names a macOS **call-in** node.
///
/// On macOS `open()` on a `tty.*` node blocks until carrier detect, which a
/// USB-TTL adapter never asserts — so it hangs forever with no error, which is
/// indistinguishable from a board that never booted. Returning the corrected path
/// rather than a bare "bad device" lets the caller name the fix.
///
/// Keys on the `tty.` prefix **including the dot**: Linux's `ttyUSB0` / `ttyACM0`
/// are call-out nodes and must pass through untouched.
#[must_use]
pub fn call_out_alternative(device: &str) -> Option<String> {
    let (dir, name) = device.rsplit_once('/')?;
    let rest = name.strip_prefix("tty.")?;
    Some(format!("{dir}/cu.{rest}"))
}

/// Makes an idle serial port look like a stream that simply has no bytes *yet*.
///
/// `protocol::stream::decode_stream` ends the session on both of the things an
/// idle port routinely does: it propagates any `Err` from `read`, and it treats
/// `Ok(0)` as a clean EOF. A board between heartbeats produces exactly those, so
/// without this adapter a reader would exit on the first quiet gap and call it
/// end-of-stream.
///
/// Absorbing *only* idleness is the point — a genuine failure (the adapter
/// unplugged, the driver gone) still propagates with its kind intact, so a dead
/// port never masquerades as a quiet one.
///
/// **The port must be opened with a read timeout** ([`READ_TIMEOUT`]). That
/// timeout is what paces this loop: it turns "no data" into a periodic `TimedOut`
/// rather than a spin. Opened without one, a port that returns `Ok(0)`
/// immediately would busy-loop here.
pub struct SerialReader<R> {
    port: R,
}

impl<R> SerialReader<R> {
    pub const fn new(port: R) -> Self {
        Self { port }
    }
}

impl<R: Read> Read for SerialReader<R> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        loop {
            match self.port.read(out) {
                // Idle, both spellings: no bytes yet, and the port is still open.
                // Falling out of the match re-enters the loop, which is the retry.
                Ok(0) => {}
                Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {}
                other => return other,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{SerialReader, call_out_alternative};
    use std::io::Read;

    use protocol::stream::{OnDecodeError, OwnedFrame, decode_stream};
    use protocol::{Frame, SpanId};

    /// A serial port scripted read-by-read. `Ok(bytes)` delivers them (empty = a
    /// zero-length read), `Err` is what the driver reported.
    ///
    /// Reading past the end **panics**, deliberately. Every script ends with an
    /// error the adapter is required to propagate, so falling off the end means the
    /// adapter absorbed something it should have returned. Panicking turns that
    /// into a fast, legible failure — returning another error instead would let an
    /// over-absorbing adapter loop forever, and the suite would hang rather than
    /// tell you what broke.
    struct MockPort {
        steps: std::collections::VecDeque<std::io::Result<Vec<u8>>>,
    }

    fn port_over(steps: Vec<std::io::Result<Vec<u8>>>) -> SerialReader<MockPort> {
        SerialReader::new(MockPort { steps: steps.into_iter().collect() })
    }

    impl Read for MockPort {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            match self.steps.pop_front() {
                Some(Ok(bytes)) => {
                    buf[..bytes.len()].copy_from_slice(&bytes);
                    Ok(bytes.len())
                }
                Some(Err(e)) => Err(e),
                None => panic!(
                    "read past the end of the scripted port — the adapter absorbed an error it \
                     was required to propagate"
                ),
            }
        }
    }

    fn wire_of(frame: &Frame<'_>) -> Vec<u8> {
        let mut buf = [0u8; 128];
        protocol::wire_encode(frame, &mut buf).expect("encode").to_vec()
    }

    /// The failure this adapter exists to prevent: a board is quiet between
    /// heartbeats, the port's read timeout fires, and the reader treats it as
    /// end-of-stream and exits mid-session. `decode_stream` propagates any `Err`
    /// from `read`, so without an adapter absorbing `TimedOut` the session dies on
    /// the first quiet gap.
    #[test]
    fn a_read_timeout_is_not_end_of_stream() {
        let a = Frame::Hello { timebase_hz: 10_000_000, protocol_version: protocol::PROTOCOL_VERSION };
        let b = Frame::SpanEnd { id: SpanId(7), t: 42 };

        let mut got = Vec::new();
        let err = decode_stream(
            &mut port_over(vec![
                Ok(wire_of(&a)),
                Err(std::io::Error::from(std::io::ErrorKind::TimedOut)),
                Ok(wire_of(&b)),
                Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe)),
            ]),
            OnDecodeError::Resync,
            |f| got.push(OwnedFrame::from_borrowed(f)),
        )
        .expect_err("the stream ends on the disconnect, not on the timeout");

        assert_eq!(
            got,
            vec![OwnedFrame::from_borrowed(&a), OwnedFrame::from_borrowed(&b)],
            "both frames must arrive — the one after the quiet gap is the point"
        );
        assert_eq!(err.kind(), std::io::ErrorKind::BrokenPipe);
    }

    /// Same contract for a zero-length read. `decode_stream` treats `Ok(0)` as a
    /// clean EOF, which is right for a file and wrong for a port that is merely
    /// idle — the board has not gone away, it just has nothing to say yet.
    #[test]
    fn a_zero_length_read_is_not_end_of_stream() {
        let a = Frame::Hello { timebase_hz: 10_000_000, protocol_version: protocol::PROTOCOL_VERSION };
        let b = Frame::SpanEnd { id: SpanId(9), t: 99 };

        let mut got = Vec::new();
        let _ = decode_stream(
            &mut port_over(vec![
                Ok(wire_of(&a)),
                Ok(Vec::new()),
                Ok(wire_of(&b)),
                Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe)),
            ]),
            OnDecodeError::Resync,
            |f| got.push(OwnedFrame::from_borrowed(f)),
        );

        assert_eq!(
            got,
            vec![OwnedFrame::from_borrowed(&a), OwnedFrame::from_borrowed(&b)],
            "an idle read must not end the session"
        );
    }

    /// The other half: absorbing *every* error would make an unplugged adapter
    /// look like an eternally quiet board, which is the same lie in the opposite
    /// direction. A genuine failure must still surface, with its kind intact.
    #[test]
    fn a_disconnected_port_propagates_its_error() {
        let err = decode_stream(
            &mut port_over(vec![Err(std::io::Error::from(std::io::ErrorKind::NotConnected))]),
            OnDecodeError::Resync,
            |_| {},
        )
        .expect_err("a disconnect is a failure, not a clean end of stream");
        assert_eq!(err.kind(), std::io::ErrorKind::NotConnected);
    }

    /// On macOS a `tty.*` node is the *call-in* device: `open()` blocks until
    /// carrier detect, which a USB-TTL adapter never asserts, so it hangs forever
    /// with no error — indistinguishable from a board that never booted. The
    /// `cu.*` node is the call-out device and skips the wait. Naming the exact
    /// replacement path is the difference between an error and a fix.
    #[test]
    fn a_call_in_device_names_its_call_out_alternative() {
        assert_eq!(
            call_out_alternative("/dev/tty.usbserial-A50285BI").as_deref(),
            Some("/dev/cu.usbserial-A50285BI")
        );
    }

    #[test]
    fn a_call_out_device_is_accepted_as_is() {
        assert_eq!(call_out_alternative("/dev/cu.usbserial-A50285BI"), None);
    }

    /// The check keys on the macOS `tty.` prefix — **with the dot**. Linux names
    /// its perfectly-good serial nodes `ttyUSB0` / `ttyACM0`, and a naive
    /// "contains tty" test would refuse the only devices that work there.
    #[test]
    fn a_linux_serial_node_is_not_a_call_in_device() {
        assert_eq!(call_out_alternative("/dev/ttyUSB0"), None);
        assert_eq!(call_out_alternative("/dev/ttyACM0"), None);
    }
}
