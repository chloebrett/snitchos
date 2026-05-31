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
