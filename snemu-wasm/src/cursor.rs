//! Turning snemu's cumulative device buffers into an incremental stream.
//!
//! `Machine::uart_output()` and `Machine::virtio_tx_output()` both return the
//! **whole** buffer every call — `uart.rs:51` is a `Vec<u8>` that only ever
//! `push`es (`uart.rs:88`). A page that wrote all of it on every animation frame
//! would redraw the entire boot log sixty times a second. So the embedder has to
//! remember what it has already handed out; this is that memory, and nothing else.

/// How much of a cumulative buffer has already been handed out.
///
/// One per buffer — the UART and the virtio telemetry stream advance
/// independently, so they must not share a cursor.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    consumed: usize,
}

impl Cursor {
    /// A cursor that has seen nothing yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The bytes appended to `buf` since the last call.
    ///
    /// Total, never panicking: a `buf` shorter than what has already been consumed
    /// means the buffer was replaced rather than appended to (a new `Machine`), so
    /// the cursor re-syncs to the new buffer and reports nothing new. Saturating
    /// rather than indexing is the whole reason this is a type and not a
    /// subtraction at the call site.
    pub fn drain<'a>(&mut self, buf: &'a [u8]) -> &'a [u8] {
        let start = self.consumed.min(buf.len());
        self.consumed = buf.len();
        &buf[start..]
    }
}

#[cfg(test)]
mod tests {
    use super::Cursor;

    /// The first drain has nothing to skip, so it yields the whole buffer.
    #[test]
    fn a_fresh_cursor_drains_everything() {
        let mut c = Cursor::new();
        assert_eq!(c.drain(b"hello"), b"hello");
    }

    /// Draining twice without the guest producing anything yields nothing the
    /// second time. This is the property that stops the page redrawing the boot log
    /// on every idle frame.
    #[test]
    fn draining_an_unchanged_buffer_again_yields_nothing() {
        let mut c = Cursor::new();
        c.drain(b"hello");
        assert_eq!(c.drain(b"hello"), b"");
    }

    /// Only what arrived since last time.
    #[test]
    fn a_drain_after_an_append_yields_only_the_new_bytes() {
        let mut c = Cursor::new();
        c.drain(b"hello");
        assert_eq!(c.drain(b"hello world"), b" world");
    }

    /// The property that matters over a whole boot: no byte is ever lost, repeated,
    /// or reordered, however the appends happen to be chunked.
    #[test]
    fn the_drains_concatenated_reproduce_the_buffer_exactly() {
        let whole = b"the quick brown fox jumps over the lazy dog";
        let mut c = Cursor::new();
        let mut seen = Vec::new();
        // Grow the buffer in uneven steps, as a guest emitting at its own pace would.
        for len in [0, 1, 2, 2, 7, 20, 21, 41, whole.len()] {
            seen.extend_from_slice(c.drain(&whole[..len]));
        }
        assert_eq!(seen, whole);
    }

    /// An empty buffer is not a special case.
    #[test]
    fn draining_an_empty_buffer_yields_nothing() {
        let mut c = Cursor::new();
        assert_eq!(c.drain(b""), b"");
    }

    /// A *shrinking* buffer is reachable by design, not a hypothetical: the page is
    /// meant to grow a control that reboots into a different `workload=`, and a new
    /// `Machine` starts with an empty output buffer. Indexing from a stale offset
    /// would panic, and a panic inside a `requestAnimationFrame` callback is a dead
    /// tab — so the cursor re-syncs instead.
    #[test]
    fn a_buffer_that_shrank_yields_nothing_and_resyncs() {
        let mut c = Cursor::new();
        c.drain(b"output from the previous machine");
        assert_eq!(c.drain(b""), b"", "a replaced machine has produced nothing yet");
        assert_eq!(c.drain(b"new"), b"new", "and the cursor tracks the new buffer from 0");
    }

    /// The same cursor type serves the virtio telemetry buffer, which is bytes with
    /// no text structure at all — so nothing about it may assume UTF-8 or lines.
    #[test]
    fn it_carries_arbitrary_bytes_not_just_text() {
        let mut c = Cursor::new();
        c.drain(&[0x00, 0xff]);
        assert_eq!(c.drain(&[0x00, 0xff, 0x80, 0x0a]), &[0x80, 0x0a]);
    }
}
