//! Where the collector reads its frame stream from.
//!
//! `decode_stream` is generic over `R: Read`, so every source is the *same*
//! decode loop over a different byte producer — the live kernel socket, a
//! recorded file (`--replay`), a UDP port (`--udp`), and the board's UART
//! (`--serial`). This module is the seam: resolve a [`Source`] from the CLI, ask
//! it for a `Read` and its decode policy, and hand both to [`run_source`].
//!
//! The bet held: four transports, one decode loop. Each arrival was a variant and
//! a `Read` impl, never a second loop. The serial one needed a `SerialReader` on
//! top, not because the seam leaked but because a physical port expresses "no
//! bytes yet" in two ways [`decode_stream`] reads as end-of-stream.
//!
//! Native only: opening a socket or a file is host I/O the wasm front-end doesn't
//! do (it feeds an in-memory buffer straight to `decode_stream`).

use std::io::Read;
use std::net::UdpSocket;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

use protocol::Frame;
use protocol::stream::{DecodeSummary, OnDecodeError, decode_stream};

/// A byte source for the frame stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// The live kernel telemetry socket — the default.
    Socket(PathBuf),
    /// Replay a previously recorded wire stream from a file.
    Replay(PathBuf),
    /// A UDP port the board/QEMU streams telemetry datagrams to (M2.5). Each
    /// datagram is a COBS batch; the payloads concatenate into one frame stream.
    Udp(u16),
    /// A physical serial line — the VisionFive 2's UART (M2). The board's own
    /// transport, and the only one where the bytes cross a wire we do not control.
    Serial(SerialConfig),
}

/// Which serial line, and how fast.
///
/// Baud is part of the identity, not a tuning knob: at the wrong rate the port
/// opens happily and delivers garbage, so it belongs anywhere the device does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SerialConfig {
    /// The device path — a **call-out** (`cu.*`) node on macOS.
    pub device: String,
    /// Line rate. 115200 is B3 Step 6's measured choice: steady-state telemetry
    /// runs ~5.5 KB/s against this rate's ~11.5 KB/s.
    pub baud: u32,
}

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
    Some(std::format!("{dir}/cu.{rest}"))
}

/// The source flags as the CLI received them, before resolution.
///
/// A struct rather than positional parameters because the transports are all
/// `Option`s of small types: `resolve(replay, udp, socket)` puts two transposable
/// arguments side by side, and a third lands with `--serial`. Named fields make a
/// swap a compile error instead of a silently wrong transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceSelection {
    /// `--replay <file>`: a recorded wire stream.
    pub replay: Option<PathBuf>,
    /// `--udp <port>`: the `net=` datagram transport.
    pub udp: Option<u16>,
    /// `--serial <dev> --baud <n>`: the board's physical UART.
    pub serial: Option<SerialConfig>,
    /// The live telemetry socket — the fallback when no source flag is given.
    pub socket: PathBuf,
}

impl Source {
    /// Resolve the source from the CLI: `--replay <file>` replays that file,
    /// else `--udp <port>` listens for datagrams, else the live `socket`.
    ///
    /// **Total by design.** The CLI's `ArgGroup` makes more than one source
    /// unreachable through the binary, but this is a library function and must be
    /// defined for every input, so the precedence stays: replay wins (offline
    /// analysis is the deliberate override), then serial, then UDP, then the socket.
    #[must_use]
    pub fn resolve(selection: SourceSelection) -> Self {
        let SourceSelection { replay, udp, serial, socket } = selection;
        match (replay, serial, udp) {
            (Some(path), _, _) => Source::Replay(path),
            (None, Some(config), _) => Source::Serial(config),
            (None, None, Some(port)) => Source::Udp(port),
            (None, None, None) => Source::Socket(socket),
        }
    }

    /// The decode policy this transport wants. A socket is lossless — an
    /// undecodable frame is a real bug, so fail. A replay is tolerant: the
    /// recording may be *of* a lossy serial capture, and replay should reproduce
    /// what it can rather than abort on the first bad frame, so resync.
    #[must_use]
    pub fn policy(&self) -> OnDecodeError {
        match self {
            Source::Socket(_) => OnDecodeError::Fail,
            // UDP is lossy — a dropped/reordered datagram is expected, and the
            // next datagram begins on a frame boundary, so resync recovers. A
            // serial line is lossy for reasons no software prevents (noise, an
            // overrun, a board resetting mid-frame), and COBS resyncs at the next
            // delimiter — which is what the framing is for.
            Source::Replay(_) | Source::Udp(_) | Source::Serial(_) => OnDecodeError::Resync,
        }
    }

    /// Open the byte stream. `Box<dyn Read>` so one decode loop drives any source.
    ///
    /// # Errors
    /// The socket connection or file open failing.
    pub fn open(&self) -> std::io::Result<Box<dyn Read>> {
        match self {
            Source::Socket(path) => Ok(Box::new(UnixStream::connect(path)?)),
            Source::Replay(path) => Ok(Box::new(std::fs::File::open(path)?)),
            Source::Udp(port) => Ok(Box::new(UdpReader::new(UdpSocket::bind(("0.0.0.0", *port))?))),
            Source::Serial(SerialConfig { device, baud }) => {
                if let Some(alternative) = call_out_alternative(device) {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        std::format!(
                            "{device} is a call-in device — opening it blocks until carrier \
                             detect, which a USB-TTL adapter never asserts. Use {alternative}."
                        ),
                    ));
                }
                let port = serialport::new(device, *baud)
                    .timeout(SERIAL_READ_TIMEOUT)
                    .open()
                    .map_err(|e| std::io::Error::other(std::format!("opening {device}: {e}")))?;
                Ok(Box::new(SerialReader::new(port)))
            }
        }
    }

    /// A short label for the connecting log line.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Source::Socket(path) => std::format!("socket {}", path.display()),
            Source::Replay(path) => std::format!("replay {}", path.display()),
            Source::Udp(port) => std::format!("udp :{port}"),
            Source::Serial(SerialConfig { device, baud }) => {
                std::format!("serial {device} @ {baud}")
            }
        }
    }
}

/// A source of datagram payloads. [`UdpSocket`] is the live one; the injection
/// seam lets the stream-assembly logic be tested without binding a real socket
/// (which stalls ad-hoc-signed test binaries under the macOS firewall).
trait RecvDatagram {
    /// Receive the next datagram's payload into `buf`, returning its length.
    /// Blocks for a live socket; `Ok(0)` signals no more datagrams (stream end).
    ///
    /// # Errors
    /// Whatever the underlying transport's receive fails with.
    fn recv_datagram(&mut self, buf: &mut [u8]) -> std::io::Result<usize>;
}

impl RecvDatagram for UdpSocket {
    // Thin delegation to `UdpSocket::recv` — I/O glue, exercised at the logic
    // level through the mock in tests, not unit-testable itself.
    #[cfg_attr(test, mutants::skip)]
    fn recv_datagram(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.recv(buf)
    }
}

/// Receive buffer for one datagram. Telemetry datagrams are MTU-bounded (a
/// batch payload ≤ ~1.5 KB), so this has ample margin; an oversized datagram is
/// truncated by `recv` and resync recovers.
const MAX_DATAGRAM: usize = 2048;

/// The serial port's read timeout — what paces [`SerialReader`]'s idle loop.
///
/// It is not a deadline on the board: an idle port must not busy-wait, and a
/// responsive Ctrl-C wants the loop to come up for air regularly. This value is
/// the only thing standing between "quiet board" and "pinned CPU core".
const SERIAL_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(200);

/// Presents a datagram source as a byte [`Read`] stream by concatenating payloads.
/// Each datagram is a COBS batch ending on a frame boundary, so the concatenation
/// is a clean COBS stream for [`decode_stream`]; `read` pulls the next datagram
/// when its buffer is drained (blocking for a live socket — no EOF).
struct DatagramReader<D> {
    source: D,
    buf: Vec<u8>,
    pos: usize,
}

/// The live UDP reader: a [`DatagramReader`] over a [`UdpSocket`].
type UdpReader = DatagramReader<UdpSocket>;

/// Makes an idle serial port look like a stream that simply has no bytes *yet*.
///
/// [`decode_stream`] ends the session on both of the things an idle port routinely
/// does: it propagates any `Err` from `read`, and it treats `Ok(0)` as a clean EOF.
/// A board between heartbeats produces exactly those, so without this adapter the
/// collector would exit on the first quiet gap and call it end-of-stream.
///
/// Absorbing *only* idleness is the point — a genuine failure (the adapter
/// unplugged, the driver gone) still propagates with its kind intact, so a dead
/// port never masquerades as a quiet one.
///
/// **The port must be opened with a read timeout.** That timeout is what paces this
/// loop: it turns "no data" into a periodic `TimedOut` rather than a spin. Opened
/// without one, a port that returns `Ok(0)` immediately would busy-loop here.
struct SerialReader<R> {
    port: R,
}

impl<R> SerialReader<R> {
    fn new(port: R) -> Self {
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

impl<D> DatagramReader<D> {
    fn new(source: D) -> Self {
        Self { source, buf: Vec::new(), pos: 0 }
    }
}

impl<D: RecvDatagram> Read for DatagramReader<D> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if self.pos >= self.buf.len() {
            let mut datagram = [0u8; MAX_DATAGRAM];
            let n = self.source.recv_datagram(&mut datagram)?;
            self.buf.clear();
            self.buf.extend_from_slice(&datagram[..n]);
            self.pos = 0;
        }
        let avail = &self.buf[self.pos..];
        let take = avail.len().min(out.len());
        out[..take].copy_from_slice(&avail[..take]);
        self.pos += take;
        Ok(take)
    }
}

/// Open `source`, decode its frames, and hand each to `on_frame`. Returns the
/// run's [`DecodeSummary`] (the resync count is telemetry about the transport).
///
/// The one place source, policy, and decode meet — so `main` and the tests drive
/// the same wiring rather than re-assembling it.
///
/// # Errors
/// Opening the source, or (under [`OnDecodeError::Fail`]) an undecodable frame.
pub fn run_source(
    source: &Source,
    on_frame: impl FnMut(&Frame<'_>),
) -> std::io::Result<DecodeSummary> {
    let mut reader = source.open()?;
    decode_stream(&mut reader, source.policy(), on_frame)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    use protocol::SpanId;
    use protocol::stream::OwnedFrame;

    /// A selection naming only the fallback socket. Tests override one field, so
    /// what a case is *about* is the line that differs from this.
    fn selection() -> SourceSelection {
        SourceSelection {
            replay: None,
            udp: None,
            serial: None,
            socket: PathBuf::from("/tmp/sock"),
        }
    }

    #[test]
    fn resolve_without_replay_uses_the_socket() {
        let s = Source::resolve(selection());
        assert_eq!(s, Source::Socket(PathBuf::from("/tmp/sock")));
    }

    #[test]
    fn resolve_with_replay_uses_the_file() {
        let s = Source::resolve(SourceSelection {
            replay: Some(PathBuf::from("/rec.bin")),
            ..selection()
        });
        assert_eq!(s, Source::Replay(PathBuf::from("/rec.bin")));
    }

    /// `resolve` stays **total**: the CLI's `ArgGroup` makes two sources
    /// unreachable, but a library function must still be defined for the input.
    /// Replay wins — offline analysis is the deliberate override. Nothing can
    /// reach this through the binary; it is pinned so the precedence cannot drift
    /// silently if the group is ever relaxed.
    #[test]
    fn resolve_prefers_replay_when_more_than_one_source_is_set() {
        let s = Source::resolve(SourceSelection {
            replay: Some(PathBuf::from("/rec.bin")),
            udp: Some(9000),
            ..selection()
        });
        assert_eq!(s, Source::Replay(PathBuf::from("/rec.bin")));
    }

    #[test]
    fn describe_names_the_transport_and_the_path() {
        // The log line must let a reader tell which source (and which file/socket)
        // the collector is on — the whole point of printing it at startup.
        let socket = Source::Socket(PathBuf::from("/tmp/snitch.sock")).describe();
        assert!(socket.contains("socket") && socket.contains("/tmp/snitch.sock"), "{socket}");
        let replay = Source::Replay(PathBuf::from("/rec/boot.bin")).describe();
        assert!(replay.contains("replay") && replay.contains("/rec/boot.bin"), "{replay}");
    }

    #[test]
    fn socket_fails_fast_and_replay_resyncs() {
        assert_eq!(Source::Socket(PathBuf::from("/s")).policy(), OnDecodeError::Fail);
        assert_eq!(Source::Replay(PathBuf::from("/r")).policy(), OnDecodeError::Resync);
    }

    #[test]
    fn resolve_with_udp_uses_the_udp_source() {
        let s = Source::resolve(SourceSelection { udp: Some(9000), ..selection() });
        assert_eq!(s, Source::Udp(9000));
    }

    #[test]
    fn udp_resyncs_like_a_lossy_transport() {
        assert_eq!(Source::Udp(9000).policy(), OnDecodeError::Resync);
    }

    fn serial_at(device: &str) -> SerialConfig {
        SerialConfig { device: device.to_string(), baud: 115_200 }
    }

    #[test]
    fn resolve_with_serial_uses_the_serial_source() {
        let s = Source::resolve(SourceSelection {
            serial: Some(serial_at("/dev/cu.usbserial-1")),
            ..selection()
        });
        assert_eq!(
            s,
            Source::Serial(SerialConfig { device: "/dev/cu.usbserial-1".to_string(), baud: 115_200 })
        );
    }

    #[test]
    fn serial_resyncs_like_the_lossy_physical_line_it_is() {
        // A UART drops bytes for reasons no software can prevent — noise, an
        // overrun, a board reset mid-frame. Failing the session on an undecodable
        // frame would end a capture the operator wanted to keep watching.
        assert_eq!(Source::Serial(serial_at("/dev/cu.x")).policy(), OnDecodeError::Resync);
    }

    #[test]
    fn describe_names_the_device_and_the_baud() {
        // Baud is half of "am I talking to the board correctly?" — a mismatch
        // yields garbage, not silence, so the startup line has to state it.
        let d = Source::Serial(serial_at("/dev/cu.usbserial-1")).describe();
        assert!(d.contains("serial"), "{d}");
        assert!(d.contains("/dev/cu.usbserial-1"), "{d}");
        assert!(d.contains("115200"), "{d}");
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

    #[test]
    fn describe_names_the_udp_port() {
        let d = Source::Udp(9000).describe();
        assert!(d.contains("udp") && d.contains("9000"), "{d}");
    }

    /// A queue of datagram payloads, drained one per `recv_datagram`, then EOF.
    /// Lets the assembly + decode be tested without a real socket.
    struct MockDatagrams {
        queue: std::collections::VecDeque<Vec<u8>>,
    }

    impl RecvDatagram for MockDatagrams {
        fn recv_datagram(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            match self.queue.pop_front() {
                Some(d) => {
                    buf[..d.len()].copy_from_slice(&d);
                    Ok(d.len())
                }
                None => Ok(0), // no more datagrams — stream end
            }
        }
    }

    fn reader_over(datagrams: Vec<Vec<u8>>) -> DatagramReader<MockDatagrams> {
        DatagramReader::new(MockDatagrams { queue: datagrams.into_iter().collect() })
    }

    #[test]
    fn datagram_payloads_concatenate_into_the_byte_stream() {
        use std::io::Read;
        // The new logic: consecutive datagram payloads become one contiguous
        // stream, the datagram boundaries invisible to the reader.
        let mut reader = reader_over(std::vec![std::vec![1, 2, 3], std::vec![4, 5, 6]]);
        let mut out = [0u8; 6];
        reader.read_exact(&mut out).expect("read both datagrams");
        assert_eq!(out, [1, 2, 3, 4, 5, 6], "payloads concatenate in order");
    }

    #[test]
    fn frames_decode_across_datagram_boundaries() {
        // Two frames delivered in two datagrams (one each): decode_stream over the
        // reader must recover both, proving the datagram→stream seam feeds the
        // decoder correctly. Also the empty final `recv` (EOF) terminates cleanly.
        let a = Frame::Hello { timebase_hz: 10_000_000, protocol_version: protocol::PROTOCOL_VERSION };
        let b = Frame::SpanEnd { id: SpanId(7), t: 42 };
        let mut buf = [0u8; 128];
        let da = protocol::wire_encode(&a, &mut buf).expect("encode a").to_vec();
        let db = protocol::wire_encode(&b, &mut buf).expect("encode b").to_vec();

        let mut got = Vec::new();
        let summary = decode_stream(&mut reader_over(std::vec![da, db]), OnDecodeError::Resync, |f| {
            got.push(OwnedFrame::from_borrowed(f));
        })
        .expect("decodes across datagrams");
        assert_eq!(got, std::vec![OwnedFrame::from_borrowed(&a), OwnedFrame::from_borrowed(&b)]);
        assert_eq!(summary.resyncs, 0, "clean datagrams need no resync");
    }

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

    /// The failure this whole step exists to prevent: a board is quiet between
    /// heartbeats, the port's read timeout fires, and the collector treats it as
    /// end-of-stream and exits mid-session. `decode_stream` propagates any `Err`
    /// from `read`, so without an adapter absorbing `TimedOut` the session dies on
    /// the first quiet gap.
    #[test]
    fn a_read_timeout_is_not_end_of_stream() {
        let a = Frame::Hello { timebase_hz: 10_000_000, protocol_version: protocol::PROTOCOL_VERSION };
        let b = Frame::SpanEnd { id: SpanId(7), t: 42 };

        let mut got = Vec::new();
        let err = decode_stream(
            &mut port_over(std::vec![
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
            std::vec![OwnedFrame::from_borrowed(&a), OwnedFrame::from_borrowed(&b)],
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
            &mut port_over(std::vec![
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
            std::vec![OwnedFrame::from_borrowed(&a), OwnedFrame::from_borrowed(&b)],
            "an idle read must not end the session"
        );
    }

    /// The other half: absorbing *every* error would make an unplugged adapter
    /// look like an eternally quiet board, which is the same lie in the opposite
    /// direction. A genuine failure must still surface, with its kind intact.
    #[test]
    fn a_disconnected_port_propagates_its_error() {
        let err = decode_stream(
            &mut port_over(std::vec![Err(std::io::Error::from(std::io::ErrorKind::NotConnected))]),
            OnDecodeError::Resync,
            |_| {},
        )
        .expect_err("a disconnect is a failure, not a clean end of stream");
        assert_eq!(err.kind(), std::io::ErrorKind::NotConnected);
    }

    #[test]
    fn a_garbage_prefixed_datagram_decodes_the_frame_after_it_and_resyncs() {
        // A datagram that begins mid-frame (bytes lost before it): an undecodable
        // COBS chunk, then a valid frame. The Resync policy skips to the next
        // `0x00` — the datagram's own frame boundary — and recovers.
        let good = Frame::SpanEnd { id: SpanId(5), t: 11 };
        let mut buf = [0u8; 128];
        let mut payload = std::vec![0x01u8, 0x00]; // undecodable chunk + terminator
        payload.extend_from_slice(protocol::wire_encode(&good, &mut buf).expect("encode"));

        let mut got = Vec::new();
        let summary = decode_stream(
            &mut std::io::Cursor::new(payload),
            OnDecodeError::Resync,
            |f| got.push(OwnedFrame::from_borrowed(f)),
        )
        .expect("resync tolerates a garbage prefix");
        assert_eq!(got, std::vec![OwnedFrame::from_borrowed(&good)]);
        assert_eq!(summary.resyncs, 1, "the garbage prefix was skipped and counted");
    }

    #[test]
    fn replay_reads_the_recorded_frames() {
        // Record two frames in the wire format to a real file, then replay it and
        // assert the same frames come back out — the end-to-end replay path.
        let frames = [
            Frame::Hello { timebase_hz: 10_000_000, protocol_version: protocol::PROTOCOL_VERSION },
            Frame::SpanEnd { id: SpanId(7), t: 42 },
        ];
        let mut file = tempfile::NamedTempFile::new().expect("temp file");
        let mut buf = [0u8; 128];
        for f in &frames {
            file.write_all(protocol::wire_encode(f, &mut buf).expect("encode")).expect("write");
        }
        file.flush().expect("flush");

        let source = Source::Replay(file.path().to_path_buf());
        let mut got = Vec::new();
        let summary = run_source(&source, |f| got.push(OwnedFrame::from_borrowed(f)))
            .expect("replay decodes");
        assert_eq!(
            got,
            frames.iter().map(OwnedFrame::from_borrowed).collect::<Vec<_>>(),
        );
        assert_eq!(summary.resyncs, 0, "a clean recording replays without resync");
    }

    #[test]
    fn replay_skips_a_corrupt_frame_in_the_recording() {
        // A recording of a lossy serial capture may hold a bad frame; replay's
        // Resync policy must reproduce the good frames around it, not abort.
        let good = Frame::SpanEnd { id: SpanId(3), t: 9 };
        let mut file = tempfile::NamedTempFile::new().expect("temp file");
        let mut buf = [0u8; 128];
        file.write_all(protocol::wire_encode(&good, &mut buf).expect("encode")).expect("write");
        file.write_all(&[0x01, 0x00]).expect("write bad chunk"); // undecodable frame
        file.write_all(protocol::wire_encode(&good, &mut buf).expect("encode")).expect("write");
        file.flush().expect("flush");

        let source = Source::Replay(file.path().to_path_buf());
        let mut count = 0;
        let summary = run_source(&source, |_| count += 1).expect("replay tolerates corruption");
        assert_eq!(count, 2, "both good frames replayed");
        assert_eq!(summary.resyncs, 1, "the bad frame was skipped and counted");
    }
}
