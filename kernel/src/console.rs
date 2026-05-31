//! Kernel console: the global UART instance plus the `print!`/`println!`
//! macros that write to it.

use crate::uart::Uart16550;

/// The kernel's console UART, initialized lazily from the DTB at boot.
///
/// Wrapping in `spin::Mutex` lets multiple call sites serialize their writes
/// once we have interrupts or SMP — today (single hart, no interrupts) it
/// never actually contends.
///
/// Known weaknesses:
/// - Accessed via the print!/println! macros, which silently fall back to a
///   hardcoded `0x10000000` if this hasn't been initialized yet. The fallback
///   only works on QEMU `virt` and any other board would lose pre-init output.
/// - No re-entrancy guard. A panic inside a print would try to lock again and
///   deadlock. Real kernels use a recursion-guarded console here.
pub static UART: spin::Once<spin::Mutex<Uart16550>> = spin::Once::new();

/// Initialize the kernel console with the given UART MMIO base address.
/// Safe to call exactly once; subsequent calls are no-ops thanks to `Once`.
///
/// # Safety
///
/// `uart_addr` must be the MMIO base of a real NS16550A-compatible UART
/// (see `Uart16550::new`).
pub unsafe fn init(uart_addr: usize) {
  UART.call_once(|| spin::Mutex::new(unsafe { Uart16550::new(uart_addr) }));
}

/// Print formatted output to the kernel console (no trailing newline).
///
/// Uses the initialized `UART` static once it's set; before that, falls back
/// to a hardcoded `Uart16550::new(0x10000000)`. The fallback is what lets the
/// panic handler still print if a panic fires during early boot.
#[macro_export]
macro_rules! print {
  ($($arg:tt)*) => {{
    use core::fmt::Write;
    if let Some(uart) = $crate::console::UART.get() {
      let _ = write!(&mut *uart.lock(), $($arg)*);
    } else {
      // SAFETY: 0x10000000 is the NS16550A MMIO base on QEMU `virt`. The
      // pre-init fallback only fires before `console::init` runs, so no
      // other writer is using the device yet.
      let mut uart = unsafe { $crate::uart::Uart16550::new(0x10000000) };
      let _ = write!(&mut uart, $($arg)*);
    }
  }};
}

/// Print formatted output to the kernel console followed by a newline.
/// Same fallback behavior as `print!`.
#[macro_export]
macro_rules! println {
  () => {{
    use core::fmt::Write;
    if let Some(uart) = $crate::console::UART.get() {
      let _ = writeln!(&mut *uart.lock());
    } else {
      // SAFETY: 0x10000000 is the NS16550A MMIO base on QEMU `virt`. The
      // pre-init fallback only fires before `console::init` runs, so no
      // other writer is using the device yet.
      let mut uart = unsafe { $crate::uart::Uart16550::new(0x10000000) };
      let _ = writeln!(&mut uart);
    }
  }};
  ($($arg:tt)*) => {{
    use core::fmt::Write;
    if let Some(uart) = $crate::console::UART.get() {
      let _ = writeln!(&mut *uart.lock(), $($arg)*);
    } else {
      // SAFETY: 0x10000000 is the NS16550A MMIO base on QEMU `virt`. The
      // pre-init fallback only fires before `console::init` runs, so no
      // other writer is using the device yet.
      let mut uart = unsafe { $crate::uart::Uart16550::new(0x10000000) };
      let _ = writeln!(&mut uart, $($arg)*);
    }
  }};
}
