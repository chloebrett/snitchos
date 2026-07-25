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

use crate::complete::{Completer, Completion, NoCompleter, menu};

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
        self.feed_with(bytes, &NoCompleter)
    }

    /// [`feed`](Self::feed), with Tab (`0x09`) asking `completer` what may come
    /// next.
    ///
    /// Tab was previously dropped with the other control bytes, so nothing that
    /// existed before changes. Its three outcomes mirror [`Completion`]:
    /// - **Forced** — exactly one spelling is legal, so type it. This asks
    ///   nothing of any model, and no model could improve on it.
    /// - **Choices** — show the menu and leave the line *exactly* as typed: a
    ///   menu is information, not an edit. The line is echoed back so the user
    ///   can carry on. (The prompt is not redrawn — the editor does not know
    ///   it; the REPL can pass one in later if it grates.)
    /// - **None** — the line is dead; say nothing rather than beep about it.
    pub fn feed_with(&mut self, bytes: &[u8], completer: &dyn Completer) -> Vec<u8> {
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
                b'\t' => self.complete_into(&mut echo, completer),
                0x20..=0x7e => {
                    self.buffer.push(byte as char);
                    echo.push(byte);
                }
                _ => {}
            }
        }
        echo
    }

    /// Apply one Tab press, appending whatever the terminal should show.
    fn complete_into(&mut self, echo: &mut Vec<u8>, completer: &dyn Completer) {
        match completer.complete_line(&self.buffer) {
            Completion::Forced(lexeme) => {
                self.buffer.push_str(&lexeme);
                echo.extend_from_slice(lexeme.as_bytes());
            }
            Completion::Choices(choices) => {
                echo.extend_from_slice(b"\r\n");
                echo.extend_from_slice(menu(&choices).as_bytes());
                echo.extend_from_slice(b"\r\n");
                echo.extend_from_slice(self.buffer.as_bytes());
            }
            Completion::None => {}
        }
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
    use crate::complete::GrammarCompleter;

    #[test]
    fn completes_a_line_on_enter() {
        let mut editor = LineEditor::new();

        let echo = editor.feed(b"hi\n");

        assert_eq!(echo, b"hi\r\n");
        assert_eq!(editor.next_line().as_deref(), Some("hi"));
        assert_eq!(editor.next_line(), None);
    }

    #[test]
    fn tab_types_a_forced_token_for_you() {
        // Only `{` can follow `use M.`, in either reading — so completing it is
        // not a guess. No model is consulted, and none could improve on it.
        let mut editor = LineEditor::new();
        editor.feed(b"use M.");

        let echo = editor.feed_with(b"\t", &GrammarCompleter);

        assert_eq!(echo, b"{");
        editor.feed(b"\n");
        assert_eq!(editor.next_line().as_deref(), Some("use M.{"));
    }

    #[test]
    fn tab_shows_a_menu_when_the_choice_is_ambiguous() {
        let mut editor = LineEditor::new();
        editor.feed(b"let x = ");

        let echo = String::from_utf8(editor.feed_with(b"\t", &GrammarCompleter)).unwrap();

        assert!(echo.contains("an integer"), "the menu is shown: {echo:?}");
        // The line is left exactly as typed — a menu is information, not an
        // edit — and echoed back so the user can carry on from where they were.
        assert!(echo.ends_with("let x = "), "the line is redrawn: {echo:?}");
        editor.feed(b"1\n");
        assert_eq!(editor.next_line().as_deref(), Some("let x = 1"));
    }

    #[test]
    fn the_editor_takes_the_completer_at_its_word() {
        // A canned answer no grammar would ever give. If the editor consulted
        // `stitch::complete` itself rather than the completer it was handed,
        // this could not pass — and the model-backed completer would then be a
        // rewrite of the editor rather than a substitution into it.
        struct Canned;
        impl crate::complete::Completer for Canned {
            fn complete_line(&self, _line: &str) -> Completion {
                Completion::Forced("!!!".into())
            }
        }

        let mut editor = LineEditor::new();
        editor.feed(b"greet");

        assert_eq!(editor.feed_with(b"\t", &Canned), b"!!!");
        editor.feed(b"\n");
        assert_eq!(editor.next_line().as_deref(), Some("greet!!!"));
    }

    #[test]
    fn tab_on_a_dead_line_does_nothing() {
        let mut editor = LineEditor::new();
        editor.feed(b"greet() { 1 } ;");

        assert_eq!(editor.feed_with(b"\t", &GrammarCompleter), b"");
    }

    #[test]
    fn feed_without_a_completer_still_ignores_tab() {
        // `feed` is the pre-completion behaviour, unchanged: control bytes are
        // dropped. Existing callers keep working without opting in.
        let mut editor = LineEditor::new();

        let echo = editor.feed(b"a\tb");

        assert_eq!(echo, b"ab");
        editor.feed(b"\n");
        assert_eq!(editor.next_line().as_deref(), Some("ab"));
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
