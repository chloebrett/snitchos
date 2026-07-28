//! `snemu boot --interactive`: a terminal on the guest's UART.
//!
//! Binary-only (the lib compiles to wasm, where there is no tty and no argv), and
//! deliberately thin. Three things stand between a keystroke and the guest, and
//! only one of them is logic:
//!
//! 1. **Raw mode.** Without it the host terminal's line discipline eats the very
//!    keys an interactive session is for — Tab gets swallowed or filename-completed
//!    by the *host* shell's habits, and nothing reaches the guest until Enter. See
//!    [`RawMode`].
//! 2. **Streaming output.** Batch snemu prints `uart_output()` once, at the end.
//!    That is unusable to type against, so the loop flushes the new tail as it
//!    appears — [`unshown`], which is the only part with a decision in it, and so
//!    the only part with tests.
//! 3. **Non-blocking reads.** The step loop must not stall waiting for a key.

use std::io::Write;

/// The bytes of `output` that have not been shown yet, given `shown` already
/// printed.
///
/// Total by construction: a `shown` past the end yields nothing rather than
/// panicking. That is not defensive padding — `uart_output()` is a growing buffer
/// owned by the machine, and a caller that resets or replays one would otherwise
/// turn a cosmetic bookkeeping slip into a crash mid-session.
#[must_use]
pub fn unshown(output: &[u8], shown: usize) -> &[u8] {
    output.get(shown..).unwrap_or(&[])
}

/// Second way out: **Ctrl-]** (`0x1d`), telnet's escape.
///
/// A *second* way, not the only one — `ISIG` stays on, so Ctrl-C kills snemu the
/// way it kills everything else. The first cut of this turned `ISIG` off, reasoning
/// that Ctrl-C belongs to the guest (the Stitch REPL's `:stim` exits on it). That
/// reasoning was fine and the trade was not: it made the escape hatch depend on the
/// input path working, and the first time input *didn't* work the session could only
/// be ended by killing it from another terminal. A debugging tool must be
/// interruptible when it is misbehaving, which is exactly when its own key handling
/// cannot be trusted. Passing Ctrl-C to the guest can come back as an opt-in flag
/// when something needs it.
pub const QUIT_BYTE: u8 = 0x1d;

/// Puts the terminal in raw mode for as long as it is alive, and restores the
/// previous settings on drop — including on panic, which is why this is a guard
/// and not a pair of calls. A snemu that left the terminal raw would leave the
/// user's shell without echo or line editing, and the fix (`stty sane`, blind)
/// is not obvious to someone who has just seen a crash.
pub struct RawMode {
    previous: libc::termios,
}

impl RawMode {
    /// Enter raw mode on stdin. `None` if stdin is not a terminal (piped input, a
    /// CI run) — in which case the caller still works, it simply has nothing to
    /// restore.
    #[must_use]
    pub fn enter() -> Option<Self> {
        // SAFETY: `isatty` on a fd we own; no memory involved.
        if unsafe { libc::isatty(libc::STDIN_FILENO) } != 1 {
            return None;
        }
        // SAFETY: `tcgetattr` fills a `termios` we own. Zeroed first so a partial
        // fill cannot leave it uninitialised.
        let mut previous: libc::termios = unsafe { core::mem::zeroed() };
        if unsafe { libc::tcgetattr(libc::STDIN_FILENO, &raw mut previous) } != 0 {
            return None;
        }

        let mut raw = previous;
        // `cfmakeraw`'s effect, and each clause earns its place here:
        // - `ECHO` off: the guest echoes typed characters itself (that is what the
        //   Stitch line editor's echo *is*), so leaving host echo on double-prints
        //   every keystroke.
        // - `ICANON` off: deliver keys as they are struck rather than at Enter —
        //   without this, Tab never arrives on its own, which is the whole feature.
        // - `IXON` off: Ctrl-S/Ctrl-Q are ordinary bytes, not host flow control.
        // - `ICRNL` off: pass Enter through as CR, exactly as a real UART would;
        //   the guest's line editor accepts CR and LF alike.
        //
        // `ISIG` stays **on** deliberately — Ctrl-C must always kill snemu. See
        // [`QUIT_BYTE`] for why that outranks passing Ctrl-C to the guest.
        raw.c_lflag &= !(libc::ECHO | libc::ICANON);
        raw.c_iflag &= !(libc::IXON | libc::ICRNL);
        // A read returns as soon as any byte is available, and never blocks waiting
        // for a minimum count — the step loop polls, it does not wait.
        raw.c_cc[libc::VMIN] = 0;
        raw.c_cc[libc::VTIME] = 0;

        // SAFETY: `tcsetattr` with a `termios` we just derived from the current one.
        if unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &raw const raw) } != 0 {
            return None;
        }
        Some(Self { previous })
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        // SAFETY: restoring the settings captured in `enter`, on the same fd.
        unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &raw const self.previous) };
    }
}

/// Read whatever the user has typed since the last call, without blocking.
///
/// Returns an empty slice when nothing is pending — the overwhelmingly common case,
/// since the guest runs millions of instructions between keystrokes.
pub fn poll_input(buf: &mut [u8]) -> &[u8] {
    // SAFETY: `read` into a buffer we own, with its true length. A non-terminal or
    // closed stdin returns <= 0, which we report as "nothing typed".
    let n = unsafe { libc::read(libc::STDIN_FILENO, buf.as_mut_ptr().cast(), buf.len()) };
    let n = usize::try_from(n).unwrap_or(0);
    &buf[..n.min(buf.len())]
}

/// Print `bytes` to stdout and flush, so output appears as the guest writes it
/// rather than when a line happens to fill.
pub fn emit(bytes: &[u8]) {
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(bytes);
    let _ = out.flush();
}

#[cfg(test)]
mod tests {
    use super::{QUIT_BYTE, unshown};

    #[test]
    fn nothing_new_shows_nothing() {
        assert!(unshown(b"boot log", 8).is_empty());
    }

    #[test]
    fn only_the_bytes_written_since_last_time_are_shown() {
        // The guest's UART buffer only grows, so "what is new" is a suffix — and
        // re-printing the whole buffer each poll (the obvious wrong version) would
        // repaint the entire boot log on every keystroke.
        assert_eq!(unshown(b"stitch> let x =", 8), b"let x =");
    }

    #[test]
    fn an_empty_buffer_shows_nothing() {
        assert!(unshown(b"", 0).is_empty());
    }

    #[test]
    fn a_cursor_past_the_end_shows_nothing_rather_than_panicking() {
        // Can't arise from a monotonically growing buffer, but a mid-session panic
        // would leave the terminal raw and the user without echo, so the total
        // function is worth more than the assertion.
        assert!(unshown(b"short", 99).is_empty());
    }

    #[test]
    fn the_quit_key_is_a_second_escape_not_the_only_one() {
        // Ctrl-] is the in-band way out. It must not be Ctrl-C, because Ctrl-C is
        // handled out-of-band by `ISIG` (left on) — a tool whose only escape runs
        // through its own input path cannot be stopped when that path is broken.
        assert_eq!(QUIT_BYTE, 0x1d);
        assert_ne!(QUIT_BYTE, 0x03);
    }
}
