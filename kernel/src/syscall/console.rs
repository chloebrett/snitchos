//! Console syscalls: `ConsoleRead` drains buffered UART input to U-mode;
//! `ConsoleWrite` writes U-mode bytes to the UART terminal. Both ambient
//! (using the console terminal is not an authority, like `DebugWrite` /
//! `Yield`); capability-mediated console access is the Tier-1 server story.

use crate::trap::TrapFrame;

/// Largest number of bytes one `ConsoleRead` returns — the stack scratch the
/// drained bytes pass through before the copy to user. The client loops to read
/// more.
const MAX_READ: usize = 256;

/// Drain up to `a1` buffered console-input bytes into the caller's buffer at
/// `a0`; returns the count in `a0` (0 if nothing is buffered — non-blocking), or
/// `u64::MAX` on a bad/unwritable user range. Ungated, like `DebugWrite`.
pub(super) fn handle_console_read(frame: &mut TrapFrame) {
    use protocol::RefusalReason;
    use snitchos_abi::Syscall;

    let sc = Syscall::ConsoleRead as u8;
    let ptr = frame.a0 as usize;
    let len = (frame.a1 as usize).min(MAX_READ);

    // Validate the destination *before* draining, so a bad pointer doesn't
    // consume (and lose) buffered input.
    if !crate::mmu::user_range_writable(ptr, len) {
        super::refuse(frame, sc, RefusalReason::BadUserRange);
        return;
    }

    let mut buf = [0u8; MAX_READ];
    let n = crate::console::read_into(&mut buf[..len]);
    // `n <= len`, and `[ptr, len)` was just validated writable, so this can't
    // fail — but honour the result rather than assume it.
    let Some(written) = crate::user::copy_to_user(ptr, &buf[..n]) else {
        super::refuse(frame, sc, RefusalReason::BadUserRange);
        return;
    };
    frame.a0 = written as u64;
}

/// Write up to `a1` bytes from the caller's buffer at `a0` to the UART terminal —
/// the same console the kernel `print!`s to (UART = the human channel, distinct
/// from the `DebugWrite` telemetry stream). Copies the bytes out (range-validated,
/// SUM-guarded), validates UTF-8, writes them, and returns the count in `a0` (or
/// `u64::MAX` on a bad pointer / non-UTF-8). Ambient, the mirror of `ConsoleRead`.
/// The runtime chunks longer writes to fit `MAX_USER_STR_LEN`.
pub(super) fn handle_console_write(frame: &mut TrapFrame) {
    use kernel_core::mmu::MAX_USER_STR_LEN;
    use protocol::RefusalReason;
    use snitchos_abi::Syscall;

    let sc = Syscall::ConsoleWrite as u8;
    let mut buf = [0u8; MAX_USER_STR_LEN];
    let Some(bytes) = crate::user::copy_from_user(frame.a0 as usize, frame.a1 as usize, &mut buf)
    else {
        super::refuse(frame, sc, RefusalReason::BadUserRange);
        return;
    };
    let Ok(msg) = core::str::from_utf8(bytes) else {
        super::refuse(frame, sc, RefusalReason::BadUtf8);
        return;
    };
    // Reuse the kernel console TX path (the `UART` mutex behind `print!`), so the
    // shell shares the one terminal with the kernel log. No trailing newline —
    // the writer controls layout (escape sequences, prompts).
    crate::print!("{msg}");
    frame.a0 = bytes.len() as u64;
}
