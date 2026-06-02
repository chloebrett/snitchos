//! Abstract clock interface. The concrete `SstcClock` impl lives in
//! the kernel binary (it touches the `time` / `stimecmp` CSRs); this
//! trait is here so host tests can substitute a `FakeClock`.

/// Read the current monotonic tick count and program future timer
/// interrupts.
pub trait Clock {
    /// Monotonic ticks since boot (raw cycle counter from the `time` CSR).
    fn now(&self) -> u64;
    /// Schedule the next timer interrupt for when the cycle counter
    /// reaches `deadline`. Implicitly acks any prior pending timer.
    fn arm(&self, deadline: u64);
}
