//! `workload=console-echo` — the Tier-0 polled-console-input demo.
//!
//! Emits a one-shot `console_echo.alive` marker span (so a test knows when the
//! program is up and reading), then loops: drain buffered UART input via
//! `console_read` and echo it straight back via `debug_write` (a `Log` frame).
//! Yields between empty reads so it doesn't hog the CPU. Proves the path
//! UART → timer drain → ring → `ConsoleRead` → userspace end to end.

#![no_std]
#![no_main]

use snitchos_user::{console_read, debug_write, entry, tracer, yield_now};

#[entry]
fn main() {
    // Marker so an observer (or the itest) knows we've reached U-mode and are
    // about to start reading — bytes typed before this may be dropped.
    let _ = tracer().span("console_echo.alive");

    let mut buf = [0u8; 64];
    loop {
        let n = console_read(&mut buf);
        if n > 0 {
            debug_write(&buf[..n]);
        } else {
            // Nothing buffered — give the CPU back rather than spinning.
            yield_now();
        }
    }
}
