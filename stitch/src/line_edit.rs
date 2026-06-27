//! Console line discipline: turn a stream of raw input bytes into finished
//! lines plus the echo bytes a terminal should display. A pure, host-tested
//! helper that sits *below* the `Platform` trait — the on-target `read_line`
//! drives it; the trait deals only in finished lines. See
//! `docs/stitch-test-library-design.md`.
//!
//! **Limitation: ASCII-only.** Only printable ASCII (`0x20..=0x7e`) enters a
//! line; control bytes and any byte `>= 0x80` are dropped, so non-ASCII input
//! (multibyte UTF-8) is silently discarded rather than accumulated. Sufficient
//! for the v1 shell; proper UTF-8 sequence handling is deferred.

use alloc::collections::VecDeque;

#[allow(clippy::wildcard_imports, reason = "alloc prelude for no_std")]
use crate::prelude::*;

/// Accumulates raw input bytes across reads. Holds the partial line typed so far
/// plus a queue of lines already completed but not yet consumed — so a chunk
/// carrying several newlines (a paste) yields every line, in order, via
/// [`next_line`](Self::next_line). `feed` advances it one chunk at a time.
#[derive(Default)]
pub struct LineEditor {
    buffer: String,
    ready: VecDeque<String>,
}

impl LineEditor {
    /// A fresh editor with an empty line buffer and no queued lines.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Process a chunk of raw input, returning the bytes to echo to the terminal.
    /// Printable ASCII bytes append to the buffer and echo as typed; Enter
    /// (`\n` or `\r`) completes the current line — queueing it for
    /// [`next_line`](Self::next_line) and echoing CRLF; backspace (`0x7f`) erases
    /// the last char. All other bytes (control, `>= 0x80`) are dropped — see the
    /// module-level ASCII-only limitation. A chunk with several newlines queues
    /// several lines.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<u8> {
        let mut echo = Vec::new();
        for &byte in bytes {
            match byte {
                b'\n' | b'\r' => {
                    echo.extend_from_slice(b"\r\n");
                    self.ready.push_back(core::mem::take(&mut self.buffer));
                }
                b'\x7f' => {
                    if self.buffer.pop().is_some() {
                        echo.extend_from_slice(b"\x08 \x08");
                    }
                }
                0x20..=0x7e => {
                    self.buffer.push(byte as char);
                    echo.push(byte);
                }
                _ => {}
            }
        }
        echo
    }

    /// Pop the oldest completed line, or `None` if no line has finished since the
    /// last one was taken. The on-target `read_line` drains this between reads.
    pub fn next_line(&mut self) -> Option<String> {
        self.ready.pop_front()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completes_a_line_on_enter() {
        let mut editor = LineEditor::new();

        let echo = editor.feed(b"hi\n");

        assert_eq!(echo, b"hi\r\n");
        assert_eq!(editor.next_line().as_deref(), Some("hi"));
        assert_eq!(editor.next_line(), None);
    }

    #[test]
    fn backspace_erases_the_last_char() {
        let mut editor = LineEditor::new();

        let echo = editor.feed(b"ax\x7f\n");

        assert_eq!(echo, b"ax\x08 \x08\r\n");
        assert_eq!(editor.next_line().as_deref(), Some("a"));
    }

    #[test]
    fn backspace_on_an_empty_line_is_a_noop() {
        let mut editor = LineEditor::new();

        let echo = editor.feed(b"\x7f\n");

        assert_eq!(echo, b"\r\n");
        assert_eq!(editor.next_line().as_deref(), Some(""));
    }

    #[test]
    fn drops_control_and_non_ascii_bytes() {
        let mut editor = LineEditor::new();

        let echo = editor.feed(b"a\tb\xc3\xa9\n");

        assert_eq!(echo, b"ab\r\n");
        assert_eq!(editor.next_line().as_deref(), Some("ab"));
    }

    #[test]
    fn retains_a_partial_line_across_reads() {
        let mut editor = LineEditor::new();

        let first = editor.feed(b"hi");
        assert_eq!(first, b"hi");
        assert_eq!(editor.next_line(), None);

        let second = editor.feed(b"\n");
        assert_eq!(second, b"\r\n");
        assert_eq!(editor.next_line().as_deref(), Some("hi"));
    }

    #[test]
    fn a_multi_line_paste_yields_every_line_in_order() {
        let mut editor = LineEditor::new();

        let echo = editor.feed(b"a\nb\nc\n");

        assert_eq!(echo, b"a\r\nb\r\nc\r\n");
        assert_eq!(editor.next_line().as_deref(), Some("a"));
        assert_eq!(editor.next_line().as_deref(), Some("b"));
        assert_eq!(editor.next_line().as_deref(), Some("c"));
        assert_eq!(editor.next_line(), None);
    }

    #[test]
    fn a_paste_with_a_trailing_partial_keeps_the_remainder_buffered() {
        let mut editor = LineEditor::new();

        editor.feed(b"a\nb");

        assert_eq!(editor.next_line().as_deref(), Some("a"));
        assert_eq!(editor.next_line(), None);

        editor.feed(b"\n");
        assert_eq!(editor.next_line().as_deref(), Some("b"));
    }
}
