//! Where the collector reads its frame stream from.
//!
//! `decode_stream` is generic over `R: Read`, so every source is the *same*
//! decode loop over a different byte producer — the live kernel socket today, a
//! recorded file (`--replay`) here, a serial line at M2 (Step 10 of
//! `plans/uart-telemetry.md`). This module is the seam: resolve a [`Source`] from
//! the CLI, ask it for a `Read` and its decode policy, and hand both to
//! [`run_source`]. Adding the serial transport is a new variant, not a new loop.
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
}

impl Source {
    /// Resolve the source from the CLI: `--replay <file>` replays that file,
    /// else `--udp <port>` listens for datagrams, else the live `socket`. Replay
    /// wins (offline analysis), then UDP, then the default socket.
    #[must_use]
    pub fn resolve(replay: Option<PathBuf>, udp: Option<u16>, socket: PathBuf) -> Self {
        match (replay, udp) {
            (Some(path), _) => Source::Replay(path),
            (None, Some(port)) => Source::Udp(port),
            (None, None) => Source::Socket(socket),
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
            // next datagram begins on a frame boundary, so resync recovers.
            Source::Replay(_) | Source::Udp(_) => OnDecodeError::Resync,
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
        }
    }

    /// A short label for the connecting log line.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Source::Socket(path) => std::format!("socket {}", path.display()),
            Source::Replay(path) => std::format!("replay {}", path.display()),
            Source::Udp(port) => std::format!("udp :{port}"),
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

    #[test]
    fn resolve_without_replay_uses_the_socket() {
        let s = Source::resolve(None, None, PathBuf::from("/tmp/sock"));
        assert_eq!(s, Source::Socket(PathBuf::from("/tmp/sock")));
    }

    #[test]
    fn resolve_with_replay_uses_the_file() {
        let s = Source::resolve(Some(PathBuf::from("/rec.bin")), None, PathBuf::from("/tmp/sock"));
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
        let s = Source::resolve(None, Some(9000), PathBuf::from("/tmp/sock"));
        assert_eq!(s, Source::Udp(9000));
    }

    #[test]
    fn udp_resyncs_like_a_lossy_transport() {
        assert_eq!(Source::Udp(9000).policy(), OnDecodeError::Resync);
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
