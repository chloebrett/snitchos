#![no_std]
#![no_main]

use core::arch::{asm, global_asm};
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicBool, Ordering};
use fdt::Fdt;

mod console;
mod dtb;
mod uart;

// Pull in the boot stub (entry.S). It defines `_start`, sets up the stack
// pointer, zeros .bss, and calls `kmain`. See linker.ld for the memory layout
// it depends on (__stack_top, __bss_start, __bss_end).
global_asm!(include_str!("entry.S"));

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
///   expected nodes, we panic during the first println-able operation.
/// - We never return from this, but we don't bring up interrupts or any
///   periodic work — the kernel just `wfi`s indefinitely once init prints out.
#[unsafe(no_mangle)]
pub extern "C" fn kmain(_hart_id: usize, dtb_phys: usize) -> ! {
  let dtb = unsafe { Fdt::from_ptr(dtb_phys as *const u8) }.unwrap();

  let uart_addr = dtb::uart_addr(&dtb);
  // SAFETY: uart_addr came from the DTB's ns16550a node, which OpenSBI
  // promises is a real UART on this machine.
  unsafe { console::init(uart_addr) };

  dtb::print_info(&dtb, uart_addr);

  println!("I am alive");

  loop {
    unsafe {
      asm!("wfi");
    }
  }
}

/// Recursion guard for the panic handler. Set on entry; if already set, we
/// must already be panicking and shouldn't try to print again (formatting the
/// panic info could itself panic, leading to infinite recursion).
static PANICKING: AtomicBool = AtomicBool::new(false);

/// Panic handler. Bypasses the UART mutex to avoid deadlocking if a panic
/// fires mid-`println!` (the lock would already be held by the outer caller).
/// Uses a recursion guard so a panic-during-panic doesn't infinite-loop.
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
  if !PANICKING.swap(true, Ordering::Relaxed) {
    // First time through. Print directly to a fresh UART, no lock.
    //
    // SAFETY: 0x10000000 is the NS16550A MMIO base on QEMU `virt`. Bypassing
    // the lock means we may interleave with whatever was printing when the
    // panic fired — accepted because we're already in a fatal state.
    use core::fmt::Write;
    let mut uart = unsafe { uart::Uart16550::new(0x10000000) };
    let _ = writeln!(&mut uart, "Kernel panic: {}", info);
  }
  loop {
    unsafe {
      asm!("wfi");
    }
  }
}
