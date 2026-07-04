//! Clock syscalls: `ClockNow` reads the monotonic tick counter (the same source
//! spans are timestamped from); `ClockFreq` reports the rate those ticks advance
//! at. Both ambient, like `Yield`: reading a clock is not an authority. Together
//! they let userspace time its own work — and convert it to real time — without a
//! span round-trip or a hardcoded platform rate.

use core::sync::atomic::Ordering;

use crate::trap::TrapFrame;

/// Return the current monotonic clock tick count in `a0`. No arguments.
pub(super) fn handle_clock_now(frame: &mut TrapFrame) {
    frame.a0 = crate::trap::now_ticks();
}

/// Return the platform timebase frequency (Hz) in `a0` — the rate `ClockNow`
/// ticks advance at, so userspace can convert a tick delta to a `Duration`. No
/// arguments.
pub(super) fn handle_clock_freq(frame: &mut TrapFrame) {
    frame.a0 = crate::trap::TIMEBASE_HZ.load(Ordering::Relaxed);
}
