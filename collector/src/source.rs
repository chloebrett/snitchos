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
}

impl Source {
    /// Resolve from the `--replay` flag: a path replays that file; its absence
    /// uses the live `socket`.
    #[must_use]
    pub fn resolve(replay: Option<PathBuf>, socket: PathBuf) -> Self {
        match replay {
            Some(path) => Source::Replay(path),
            None => Source::Socket(socket),
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
            Source::Replay(_) => OnDecodeError::Resync,
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
        }
    }

    /// A short label for the connecting log line.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Source::Socket(path) => std::format!("socket {}", path.display()),
            Source::Replay(path) => std::format!("replay {}", path.display()),
        }
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
        let s = Source::resolve(None, PathBuf::from("/tmp/sock"));
        assert_eq!(s, Source::Socket(PathBuf::from("/tmp/sock")));
    }

    #[test]
    fn resolve_with_replay_uses_the_file() {
        let s = Source::resolve(Some(PathBuf::from("/rec.bin")), PathBuf::from("/tmp/sock"));
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
