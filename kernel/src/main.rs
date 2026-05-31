#![no_std]
#![no_main]

use core::panic::PanicInfo;
use core::arch::global_asm;
use core::arch::asm;
use core::fmt::Write;

global_asm!(include_str!("entry.S"));

#[allow(unused_macros)]
macro_rules! print {
  ($($arg:tt)*) => {{
    use core::fmt::Write;
    let _ = write!(&mut $crate::Uart16550::new(0x10000000), $($arg)*);
  }}
}

macro_rules! println {
  () => {{
    use core::fmt::Write;
    let _ = writeln!(&mut $crate::Uart16550::new(0x10000000));
  }};
  ($($arg:tt)*) => {{
    use core::fmt::Write;
    let _ = writeln!(&mut $crate::Uart16550::new(0x10000000), $($arg)*);
  }}
}

#[unsafe(no_mangle)]
pub extern "C" fn kmain(_hart_id: usize, dtb_phys: usize) -> ! {
  let dtb = unsafe { fdt::Fdt::from_ptr(dtb_phys as *const u8) }.unwrap();
  print_dtb_info(&dtb);

  println!("I am alive");

  loop {
    unsafe {
      asm!("wfi");
    }
  }
}

/// This function is called on panic.
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
  println!("Kernel panic: {}", info);
  loop {
    unsafe {
      asm!("wfi");
    }
  }
}

fn print_dtb_info(dtb: &fdt::Fdt) {
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

pub struct Uart16550 {
  base: usize,
}

impl Uart16550 {
  pub const fn new(base: usize) -> Self { Uart16550 { base } }

  pub fn putchar(&self, c: u8) {
    let thr_addr = self.base as *mut u8;
    let lsr_addr = (self.base + 5) as *const u8;
    unsafe {
      while lsr_addr.read_volatile() & 0b00100000 == 0 {}
      thr_addr.write_volatile(c);
    }
  }
}

impl Write for Uart16550 {
  fn write_str(&mut self, s: &str) -> core::fmt::Result {
    for byte in s.bytes() {
      self.putchar(byte);
    }
    Ok(())
  }
}