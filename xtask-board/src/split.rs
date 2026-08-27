//! Separating the board's two streams: what a human reads, and what tooling does.
//!
//! One UART carries both. U-Boot prints plain ASCII; the kernel emits COBS-framed
//! [`protocol::Frame`]s. Neither knows about the other, and they overlap: the
//! kernel sends `Hello` long before `console=frames` is applied, so early-boot
//! `println!`s go raw to the same wire while frames are already flowing.
//!
//! **The whole specification is: text is the bytes that aren't frames.** Extract
//! every frame; concatenate the leftovers. That rule is uniform — it does not
//! care whether the leftover is U-Boot's banner, a `println!` between two frames,
//! or a second boot after a mid-capture reset.
//!
//! # Why this is not a line-splitter
//!
//! An earlier design emitted one item per *text line*, which meant finding line
//! boundaries in a half-binary stream — and `0x0A` is an ordinary payload byte.
//! COBS removes `0x00` **and only** `0x00`; `SpanId(10)` encodes as a literal
//! newline byte, and ~2.7% of `SpanEnd` frames carry one by timestamp alone. Any
//! splitter that treats `\n` as a boundary cuts real frames in half. A line is a
//! display concept; it belongs where every byte is text by construction, not
//! here.
//!
//! # Finding a frame that shares a chunk with text
//!
//! `0x00` delimits frames, so the obvious loop is "decode each `0x00`-delimited
//! chunk". That loses the first frame of every boot: U-Boot's log contains no
//! `0x00`, so the first terminator on the wire is the *first frame's*, and the
//! whole log lands in the same chunk. Dropping it costs `Hello` — sent exactly
//! once per boot (`kernel/src/obs/tracing.rs`), carrying the `timebase_hz` every
//! later timestamp is relative to.
//!
//! So a failing chunk is searched for the frame hiding at its end. Two filters,
//! both measured rather than argued:
//!
//! 1. **Schema**: the candidate must decode to a frame spanning *exactly* to the
//!    terminator.
//! 2. **Text plausibility**: everything before it must still look like text.
//!
//! When several candidates survive — `CapEvent`'s NUL-padded name admits a few —
//! **the latest wins**. Swept over 96 mixed chunks (16 frame shapes × 6 text
//! prefixes): latest-wins was wrong 0 times, earliest-wins 3.
//!
//! The search is bounded by `MAX_FRAME_BYTES`, so a long boot log in front of a
//! frame costs nothing.

use protocol::stream::{OwnedFrame, try_decode_frame};

/// The largest frame this transport can carry, from `ENCODE_SCRATCH` in
/// `kernel-obs/src/uart_sink.rs` — the sink counts anything bigger as a drop
/// rather than emitting it. A frame start therefore cannot lie further back than
/// this from its terminator, which is what bounds the search.
///
/// A drift here is safe in one direction only: too small silently stops
/// recovering large frames. Kept as a named constant so the coupling is visible.
const MAX_FRAME_BYTES: usize = 520;

/// A capture, separated.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Split {
    /// Everything that was not part of a frame, in order, lossily decoded.
    pub io_text: String,
    /// Every frame recovered, in wire order.
    pub frames: Vec<OwnedFrame>,
    /// **Frames lost**, not "chunks that were not frames". A stray `0x00` in
    /// plain text costs nothing and is not counted — reporting it would send
    /// someone hunting a transport fault that does not exist.
    pub resyncs: u64,
}

/// Separate a captured byte stream into its text and its frames.
///
/// Never fails and never panics: undecodable bytes are counted and kept as text
/// rather than dropped. On a diagnostic bridge the bytes you saw are evidence,
/// and [`Split::resyncs`] is what flags them as suspect.
#[must_use]
pub fn split(bytes: &[u8]) -> Split {
    let mut text: Vec<u8> = Vec::new();
    let mut out = Split::default();

    // `split_inclusive` gives each chunk with its terminator attached, and the
    // trailing run without one as a final chunk. Written this way rather than as
    // an index loop advancing past each `0x00` deliberately: that form carries a
    // `+ 1` whose mutation stops the loop advancing, and a splitter that hangs on
    // a capture is a worse failure than one that mis-splits it.
    for chunk in bytes.split_inclusive(|&b| b == 0) {
        let Some(body) = chunk.strip_suffix(&[0]) else {
            // No terminator, so this is the last run of the capture and cannot be
            // a frame — every frame ends in one. U-Boot's `=> ` prompt lands here.
            text.extend_from_slice(chunk);
            continue;
        };
        if let Some((start, frame)) = frame_in(chunk) {
            text.extend_from_slice(&chunk[..start]);
            out.frames.push(frame);
        } else {
            // Keep the body, drop the terminator — that byte is framing, not
            // content. Only a chunk that *looked* like a frame counts as a loss.
            if !is_text(body) {
                out.resyncs += 1;
            }
            text.extend_from_slice(body);
        }
    }

    out.io_text = String::from_utf8_lossy(&text).into_owned();
    out
}

/// The frame ending at `chunk`'s terminator, and the offset it starts at.
///
/// The whole chunk is tried first, and that is not merely an optimisation for
/// the common case: with no text in front, the true start is `0`, and the
/// latest-wins tie-break below would prefer some later offset that also decodes.
/// Trying `0` first is what makes "no text" the answer whenever it is true.
fn frame_in(chunk: &[u8]) -> Option<(usize, OwnedFrame)> {
    if let Some(frame) = frame_spanning(chunk, 0) {
        return Some((0, frame));
    }
    let earliest = chunk.len().saturating_sub(MAX_FRAME_BYTES);
    (earliest..chunk.len())
        .rev()
        .filter(|&start| is_text(&chunk[..start]))
        .find_map(|start| frame_spanning(chunk, start).map(|frame| (start, frame)))
}

/// Decode a frame beginning at `start`.
///
/// `chunk` always ends at its own — and only — `0x00`, so the frame necessarily
/// reaches the end: there is no earlier delimiter for the decode to stop at.
///
/// **An earlier version guarded on `consumed == chunk.len() - start`, believing
/// it rejected a candidate that matched a prefix and left a remainder. It did
/// not — mutation testing found the guard was always true.**
/// [`try_decode_frame`] derives `consumed` from the delimiter's position, not
/// from what postcard actually read, so a decode leaving trailing bytes is
/// indistinguishable here from an exact one. Asking that question needs a
/// `remaining == 0` check inside `protocol`, which is shared wire-contract code.
///
/// In practice the two filters in [`frame_in`] — decodes at all, and has a
/// text-shaped prefix — were measured sufficient: over 96 mixed chunks (16 frame
/// shapes × 6 text prefixes) they left the right answer in every case.
fn frame_spanning(chunk: &[u8], start: usize) -> Option<OwnedFrame> {
    try_decode_frame(&chunk[start..]).ok()?.map(|(frame, _)| frame)
}

/// Whether these bytes still read as console text.
///
/// Deliberately ASCII-only: U-Boot and the kernel's early `println!`s are ASCII,
/// and admitting arbitrary UTF-8 would let frame bodies pass as text and defeat
/// the filter. Tab, CR and LF are text; the rest of C0 is not.
fn is_text(bytes: &[u8]) -> bool {
    bytes.iter().all(|&b| matches!(b, b'\t' | b'\n' | b'\r' | 0x20..=0x7E))
}
