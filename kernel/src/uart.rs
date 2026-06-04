//! Driver for the NS16550A UART (and any register-compatible clone, such as
//! the one QEMU's `virt` machine exposes via MMIO).

use core::fmt::Write;

/// Driver for an NS16550A UART at a given MMIO base address.
///
/// Storing the base as `usize` rather than `*mut u8` sidesteps `*mut`'s
/// `!Sync` default — the struct is naturally `Send + Sync` because it's just
/// an integer wearing a type.
///
/// Known weaknesses:
/// - **Polled output only.** `putchar` spins on LSR bit 5. No interrupts, no
///   FIFO use, no flow control. Fine for v0.1; the moment we have something
///   meaningful to log we'll want interrupt-driven TX.
/// - **No initialization step.** Real drivers configure baud rate (DLL/DLM
///   divisor latches), line control (LCR: bits, parity, stop), and the FIFO
///   (FCR). We rely on whatever OpenSBI configured during M-mode init.
/// - **No receive path.** We only transmit. Input handling waits until we
///   build a debug shell.
/// - **No error checking.** LSR error bits (overrun, parity, framing) are
///   ignored. A real driver surfaces them.
/// - **Multiple `Uart16550`s pointing at the same MMIO address don't
///   coordinate.** The `&self` API is correct because the struct has no
///   state to race over, but the *device* does, and `&self` doesn't help
///   there. Serialization is provided externally via `kernel::sync::Mutex<Uart16550>`.
pub struct Uart16550 {
  base: usize,
}

impl Uart16550 {
  /// Construct a driver for a UART at the given MMIO base address.
  ///
  /// # Safety
  ///
  /// `base` must be the MMIO base of a real NS16550A-compatible UART, and
  /// the caller must ensure that any other code touching the same registers
  /// either coordinates through this driver (e.g. via a shared `Mutex`) or
  /// doesn't conflict. Constructing two `Uart16550`s pointing at the same
  /// region without external coordination is undefined behavior at the
  /// device-state level (the type system can't see it).
  pub const unsafe fn new(base: usize) -> Self { Uart16550 { base } }

  /// Block until the transmit holding register is empty, then send one byte.
  ///
  /// Spins on LSR bit 5 (THRE — Transmit Holding Register Empty). At 115200
  /// baud each byte takes ~87 microseconds on the wire; the CPU spins
  /// millions of times faster, so this loop dominates transmit time.
  pub fn putchar(&self, c: u8) {
    let thr_addr = self.base as *mut u8;
    let lsr_addr = (self.base + 5) as *const u8;
    unsafe {
      while lsr_addr.read_volatile() & 0b00100000 == 0 {}
      thr_addr.write_volatile(c);
    }
  }
}

/// `core::fmt::Write` impl so the UART can back the `print!`/`println!`
/// macros. `write_str` needs `&mut self` per trait contract; we delegate to
/// `&self` `putchar` because the struct itself has no state to mutate.
impl Write for Uart16550 {
  fn write_str(&mut self, s: &str) -> core::fmt::Result {
    for byte in s.bytes() {
      self.putchar(byte);
    }
    Ok(())
  }
}
