//! Showing what was actually put on the wire.
//!
//! The bridge writes bytes to a board and then reports what came back. Without an
//! echo of the *input*, the transcript records what the caller meant rather than
//! what was sent — and those diverge exactly when it matters, because a shell,
//! `clap`, and a `--input-file` each get their own crack at the quoting first.
//!
//! The 2026-08-27 board session lost time to this: with a U-Boot line producing no
//! response, the sent bytes had to be dumped by hand with `cat -v` before the
//! quoting could be ruled out. Echoing removes that step, and it makes the *input
//! mechanism* irrelevant — argv, file or stdin all leave the same record.
//!
//! **Which is why this renders rather than dumps.** Writing the raw bytes would
//! reproduce the very unreadability it exists to fix: a `\r` would perform a
//! carriage return instead of showing one. Whether a command is submitted at all
//! turns on that invisible byte — U-Boot wants `\r`, the Stitch REPL differs — so
//! the one thing the echo must never do is hide it.

/// Render `bytes` so every one of them can be seen, `cat -v` style.
///
/// Printable ASCII passes through; control bytes become caret notation (`\r` is
/// `^M`, `\n` is `^J`, `0x7f` is `^?`); anything ≥ `0x80` becomes `\xNN`. Empty
/// input renders as `(nothing)` — sending nothing is a legitimate move ("just
/// watch"), and it should read as a deliberate choice rather than as a line of
/// output that went missing.
///
/// Lossy on purpose: this is for a human reading a transcript, not a round-trip.
#[must_use]
pub fn visible(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return "(nothing)".to_string();
    }
    let mut out = String::with_capacity(bytes.len());
    for &b in bytes {
        match b {
            0x20..=0x7e => out.push(b as char),
            // Caret notation: the control byte plus 0x40 is its printable letter,
            // so 0x0d ⇒ 'M'. `0x7f` (DEL) is the conventional `^?`.
            0x00..=0x1f => {
                out.push('^');
                out.push((b + 0x40) as char);
            }
            0x7f => out.push_str("^?"),
            _ => out.push_str(&std::format!("\\x{b:02x}")),
        }
    }
    out
}
