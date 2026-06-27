//! Clock syscall: `ClockNow` — read the monotonic tick counter (the same source
//! spans are timestamped from). Ambient, like `Yield`: reading a clock is not an
//! authority. Lets userspace time its own work without a span round-trip.

use crate::trap::TrapFrame;

/// Return the current monotonic clock tick count in `a0`. No arguments.
pub(super) fn handle_clock_now(frame: &mut TrapFrame) {
    frame.a0 = crate::trap::now_ticks();
}
