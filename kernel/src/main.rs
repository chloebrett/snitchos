#![no_std]
#![no_main]

use core::panic::PanicInfo;
use core::arch::global_asm;
use core::arch::asm;
use core::fmt::Write;
use fdt::Fdt;

// Pull in the boot stub (entry.S). It defines `_start`, sets up the stack
// pointer, zeros .bss, and calls `kmain`. See linker.ld for the memory layout
// it depends on (__stack_top, __bss_start, __bss_end).
global_asm!(include_str!("entry.S"));

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

/// Print formatted output to the kernel console (no trailing newline).
///
/// Uses the initialized `UART` static once it's set; before that, falls back
/// to a hardcoded `Uart16550::new(0x10000000)`. The fallback is what lets the
/// panic handler still print if a panic fires during early boot.
#[allow(unused_macros)]
macro_rules! print {
  ($($arg:tt)*) => {{
    use core::fmt::Write;
    if let Some(uart) = $crate::UART.get() {
      let _ = write!(&mut *uart.lock(), $($arg)*);
    } else {
      let mut uart = $crate::Uart16550::new(0x10000000);
      let _ = write!(&mut uart, $($arg)*);
    }
  }}
}

/// Print formatted output to the kernel console followed by a newline.
///
/// Same fallback behavior as `print!`. See `UART` for known weaknesses.
macro_rules! println {
  () => {{
    use core::fmt::Write;
    if let Some(uart) = $crate::UART.get() {
      let _ = writeln!(&mut *uart.lock());
    } else {
      let mut uart = $crate::Uart16550::new(0x10000000);
      let _ = writeln!(&mut uart);
    }
  }};
  ($($arg:tt)*) => {{
    use core::fmt::Write;
    if let Some(uart) = $crate::UART.get() {
      let _ = writeln!(&mut *uart.lock(), $($arg)*);
    } else {
      let mut uart = $crate::Uart16550::new(0x10000000);
      let _ = writeln!(&mut uart, $($arg)*);
    }
  }}
}

/// Kernel entry point, called from `_start` (see entry.S).
///
/// Inputs come from OpenSBI's S-mode handoff contract:
/// - `_hart_id`: which hart we booted on (we only have one in v0.1).
/// - `dtb_phys`: physical address of the device tree blob.
///
/// MMU is off, interrupts are off. We have a valid stack and a zeroed .bss.
///
/// Known weaknesses:
/// - All DTB lookups `.unwrap()`. If the DTB is malformed or missing the
///   expected nodes, we panic during the first println-able operation, which
///   itself prints through the very console we may have failed to find.
/// - We never return from this, but we don't bring up interrupts or any
///   periodic work — the kernel just `wfi`s indefinitely once init prints out.
#[unsafe(no_mangle)]
pub extern "C" fn kmain(_hart_id: usize, dtb_phys: usize) -> ! {
  let dtb = unsafe { Fdt::from_ptr(dtb_phys as *const u8) }.unwrap();

  let uart_addr = dtb_uart_addr(&dtb);
  UART.call_once(|| spin::Mutex::new(Uart16550::new(uart_addr)));

  print_dtb_info(&dtb);

  println!("I am alive");

  loop {
    unsafe {
      asm!("wfi");
    }
  }
}

/// Panic handler. Prints the panic info and parks forever.
///
/// Known weaknesses:
/// - Uses `println!`, which locks the UART. A panic that fires while the
///   UART mutex is held (e.g. from inside a `write_str`) will deadlock.
/// - No recursion guard. If formatting `info` itself panics, we recurse
///   into this handler indefinitely.
/// - On a real board with a flaky UART, the panic message may never reach
///   the user. Eventually we'd want a fallback emit path (a hardware reset
///   button is sometimes the only correct answer).
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
  println!("Kernel panic: {}", info);
  loop {
    unsafe {
      asm!("wfi");
    }
  }
}

/// Find the first `ns16550a`-compatible serial port in the DTB and return
/// its MMIO base address.
///
/// Known weaknesses:
/// - Panics on missing/malformed entries via `.unwrap()`. Fine for v0.1
///   (no DTB → no kernel), but a real driver would surface the error.
/// - Hardcodes the `"ns16550a"` compatible string. Boards that report
///   `"snps,dw-apb-uart"` or `"arm,pl011"` etc. won't match. Will need a
///   compatibility list when we add a second platform.
fn dtb_uart_addr(dtb: &Fdt) -> usize {
  let uart = dtb.find_compatible(&["ns16550a"]).unwrap();
  uart.reg().unwrap().next().unwrap().starting_address as usize
}

/// Print the load-bearing values we extract from the DTB: memory regions,
/// the CPU timebase frequency, and the UART MMIO range. Mostly a v0.1
/// sanity-check — once we wire these into the real allocator, clock, and
/// driver registry, this function goes away.
///
/// Known weaknesses:
/// - Manually decodes `timebase-frequency` because the `fdt` 0.1.5 crate
///   doesn't surface it as a typed accessor. Real DTB libraries (e.g.
///   `device_tree`) handle this; we kept the parser cheap.
/// - Re-runs `find_compatible("ns16550a")` after `dtb_uart_addr` already
///   did the same lookup. Cheap but wasted work.
fn print_dtb_info(dtb: &Fdt) {
  for region in dtb.memory().regions() {
    println!(
      "memory: {:#x} ({} bytes)",
      region.starting_address as usize,
      region.size.unwrap_or(0),
    );
  }

  let timebase = dtb
    .cpus()
    .next()
    .and_then(|c| c.properties().find(|p| p.name == "timebase-frequency"))
    .and_then(|p| {
      let bytes = p.value;
      (bytes.len() == 4).then(|| u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    })
    .unwrap_or(0);
  println!("timebase: {} Hz", timebase);

  let uart = dtb.find_compatible(&["ns16550a"]).unwrap();
  let uart_reg = uart.reg().unwrap().next().unwrap();
  println!(
    "uart: {:#x} ({} bytes)",
    uart_reg.starting_address as usize,
    uart_reg.size.unwrap_or(0),
  );
}

/// Driver for the NS16550A UART (and any register-compatible clone, such as
/// the one QEMU's `virt` machine exposes via MMIO).
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
///   there. Serialization is provided externally via `spin::Mutex<Uart16550>`.
pub struct Uart16550 {
  base: usize,
}

impl Uart16550 {
  /// Construct a driver for a UART at the given MMIO base address.
  pub const fn new(base: usize) -> Self { Uart16550 { base } }

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
