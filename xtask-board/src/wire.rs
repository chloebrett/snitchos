//! Reading a capture off a transport, whatever the transport is.
//!
//! The loop itself is four lines; the decision inside it is the crate's thesis.
//! **An idle read advances the capture, a dead one ends it** — a board between
//! heartbeats is quiet, not gone, and a transport that has died is gone, not
//! quiet. Confusing the two in either direction produces the same useless
//! symptom (no frames) with opposite causes, which is what [`crate::reach`]
//! spends a whole module keeping apart.
//!
//! Note this is the *opposite* discipline to `collector::serial::SerialReader`,
//! deliberately. That adapter absorbs idle reads so a decode loop never mistakes
//! quiet for end-of-stream; this loop must *see* them, because noticing quiet is
//! its entire job. Same two spellings of "no bytes yet", opposite correct
//! responses — which is why the port is not wrapped here.

use std::io::{Read, Write};
use std::time::Instant;

use crate::stop::{Capture, StopCondition, StopReason};

/// The transport seam: a serial port today, Phase 2's TCP socket unchanged.
pub trait ReadWrite: Read + Write {}
impl<T: Read + Write + ?Sized> ReadWrite for T {}

/// How a capture ended.
///
/// A dead transport is **not** a [`StopReason`]. Folding it into `Timeout` would
/// be true-ish and useless: the caller could no longer tell "the board never
/// answered" from "we stopped being able to hear it", which are the two halves
/// [`crate::reach`] exists to keep apart and which need opposite fixes. Mutation
/// testing is what surfaced this — with both collapsed into one reason, no
/// assertion could distinguish them, so the guard that tells them apart was
/// unkillable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ended {
    /// The capture ran to one of its conditions.
    Stopped(StopReason),
    /// The transport failed part-way through.
    TransportFailed(std::io::ErrorKind),
}

/// Read until `condition` fires, returning how it ended and everything read.
///
/// The bytes come back either way: what was read before a transport died is
/// still evidence, and on a diagnostic bridge throwing it away loses the part
/// that usually says why.
///
/// Bounded by the condition's own deadline, so no transport behaviour can make
/// this spin: a port that only ever goes idle still hits the timeout.
pub fn capture(port: &mut dyn ReadWrite, condition: &StopCondition) -> (Ended, Vec<u8>) {
    let mut capture = Capture::new(condition.clone());
    let mut buf = [0u8; 1024];
    let mut raw: Vec<u8> = Vec::new();
    let started = Instant::now();

    loop {
        let elapsed = started.elapsed();
        let stop = match port.read(&mut buf) {
            // Idle, both spellings — no bytes yet, and the port is still open.
            Ok(0) => capture.tick(elapsed),
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => capture.tick(elapsed),
            Ok(n) => {
                raw.extend_from_slice(&buf[..n]);
                capture.observe(elapsed, &buf[..n])
            }
            // A dead transport, reported as itself — see [`Ended`].
            Err(e) => return (Ended::TransportFailed(e.kind()), raw),
        };
        if let Some(reason) = stop {
            return (Ended::Stopped(reason), raw);
        }
    }
}
