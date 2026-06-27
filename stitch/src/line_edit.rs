//! Console line discipline: turn a stream of raw input bytes into finished
//! lines plus the echo bytes a terminal should display. A pure, host-tested
//! helper that sits *below* the `Platform` trait — the on-target `read_line`
//! drives it; the trait deals only in finished lines. See
//! `docs/stitch-test-library-design.md`.

#[allow(clippy::wildcard_imports, reason = "alloc prelude for no_std")]
use crate::prelude::*;

/// Accumulates raw input bytes across reads, holding the partial line typed so
/// far. `feed` advances it one chunk at a time.
#[derive(Default)]
pub struct LineEditor {
    buffer: String,
}

/// The result of feeding a chunk of input: a finished line (when Enter was seen)
/// and the bytes the terminal should echo for what was just typed.
#[derive(Debug)]
pub struct Edit {
    pub line: Option<String>,
    pub echo: Vec<u8>,
}

impl LineEditor {
    /// A fresh editor with an empty line buffer.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Process a chunk of raw input. Printable bytes append to the buffer and
    /// echo as typed; Enter (`\n` or `\r`) completes the line, echoes CRLF, and
    /// resets the buffer.
    pub fn feed(&mut self, bytes: &[u8]) -> Edit {
        let mut echo = Vec::new();
        let mut line = None;
        for &byte in bytes {
            match byte {
                b'\n' | b'\r' => {
                    echo.extend_from_slice(b"\r\n");
                    line = Some(core::mem::take(&mut self.buffer));
                }
                _ => {
                    self.buffer.push(byte as char);
                    echo.push(byte);
                }
            }
        }
        Edit { line, echo }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completes_a_line_on_enter() {
        let mut editor = LineEditor::new();

        let edit = editor.feed(b"hi\n");

        assert_eq!(edit.line.as_deref(), Some("hi"));
        assert_eq!(edit.echo, b"hi\r\n");
    }
}
