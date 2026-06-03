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
/// - Accessed via the print!/println! macros, which silently fall back to
///   `_pre_init_uart()` (via `emergency_uart_base()`) if this hasn't been
///   initialized yet. The base is hardcoded for QEMU `virt`; any other board
///   would lose pre-init output.
/// - No re-entrancy guard. A panic inside a print would try to lock again and
///   deadlock. Real kernels use a recursion-guarded console here.
pub static UART: spin::Once<spin::Mutex<Uart16550>> = spin::Once::new();

/// Initialize the kernel console with the given UART MMIO base
/// physical address (typically pulled from the DTB).
///
/// Translates to the higher-half VA before storing — the kernel runs
/// at higher-half PC after the trampoline, and `mmu::unmap_identity`
/// later tears down the identity MMIO mapping. The MMIO region is
/// dual-mapped by `mmu::enable`, so this works from the moment
/// `enable` returns.
///
/// Safe to call exactly once; subsequent calls are no-ops thanks to
/// `Once`.
///
/// # Safety
///
/// `uart_addr` must be the physical MMIO base of a real
/// NS16550A-compatible UART, and `mmu::enable` must have run (so the
/// higher-half MMIO mapping is live).
pub unsafe fn init(uart_addr: usize) {
  let va = uart_addr + crate::mmu::KERNEL_OFFSET;
  UART.call_once(|| spin::Mutex::new(unsafe { Uart16550::new(va) }));
}

/// Hardcoded NS16550A physical MMIO base for QEMU `virt`. Used via
/// `emergency_uart_base()` by both the macro fallback (`_pre_init_uart`)
/// and the panic handler. Wrong on any other board — see `console.rs`
/// known weaknesses.
pub const QEMU_VIRT_UART_BASE: usize = 0x10000000;

/// Returns a UART driver pointing at the QEMU `virt` UART. Used by
/// the `print!`/`println!` macros when `UART` isn't initialized yet.
/// (The panic handler builds its own UART directly via
/// `emergency_uart_base()` so it doesn't depend on this function.)
///
/// Picks the address space via `emergency_uart_base`: physical when
/// the MMU is off, higher-half when it's on.
///
/// # Safety
///
/// Only safe to call before `console::init` has run — no other writer
/// is using the device yet. Not exported for general use; it's `pub`
/// so the macros can reach it, not because callers should.
pub unsafe fn _pre_init_uart() -> Uart16550 {
  // SAFETY: see function-level doc; precondition is that no other code
  // currently holds the UART.
  unsafe { Uart16550::new(emergency_uart_base()) }
}

/// Pick the UART MMIO base address that's valid for the current
/// `satp` state. Used by `_pre_init_uart` and the panic handler — both
/// can fire at any boot stage, including pre-MMU and post-identity-unmap.
pub fn emergency_uart_base() -> usize {
  let satp: u64;
  // SAFETY: `csrr satp` is a non-trapping read in S-mode.
  unsafe { core::arch::asm!("csrr {}, satp", out(reg) satp) };
  if satp != 0 {
    QEMU_VIRT_UART_BASE + crate::mmu::KERNEL_OFFSET
  } else {
    QEMU_VIRT_UART_BASE
  }
}

/// Print formatted output to the kernel console (no trailing newline).
///
/// Uses the initialized `UART` static once it's set; before that, falls back
/// to `_pre_init_uart()` (which routes through `emergency_uart_base()` so
/// the right address space is picked for the current MMU state).
#[macro_export]
macro_rules! print {
  ($($arg:tt)*) => {{
    use core::fmt::Write;
    if let Some(uart) = $crate::console::UART.get() {
      let _ = write!(&mut *uart.lock(), $($arg)*);
    } else {
      // SAFETY: pre-init fallback fires before console::init runs.
      let mut uart = unsafe { $crate::console::_pre_init_uart() };
      let _ = write!(&mut uart, $($arg)*);
    }
  }};
}

/// Print formatted output to the kernel console followed by a newline.
/// Same fallback behavior as `print!`.
#[macro_export]
macro_rules! println {
  () => { $crate::print!("\n") };
  ($($arg:tt)*) => {{
    use core::fmt::Write;
    if let Some(uart) = $crate::console::UART.get() {
      let _ = writeln!(&mut *uart.lock(), $($arg)*);
    } else {
      // SAFETY: pre-init fallback fires before console::init runs.
      let mut uart = unsafe { $crate::console::_pre_init_uart() };
      let _ = writeln!(&mut uart, $($arg)*);
    }
  }};
}
