#![no_std]
#![no_main]

use core::panic::PanicInfo;
use core::arch::global_asm;
use core::arch::asm;
use core::fmt::Write;

global_asm!(include_str!("entry.S"));

#[unsafe(no_mangle)]
pub extern "C" fn kmain(_hart_id: usize, _dtb_phys: usize) -> ! {
  let _ = writeln!(&mut SbiConsole, "I am alive");

  loop {
    unsafe {
      asm!("wfi");
    }
  }
}

/// This function is called on panic.
#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
      unsafe {
        asm!("wfi");
      }
    }
}

fn sbi_putchar(c: u8) {
  unsafe {
    asm!(
      "ecall",
      in("a7") 1_usize, // legacy putchar EID
      inout("a0") c as usize => _, // byte in; return clobbered
      out("a1") _, // also clobbered by SBI
      options(nostack),
    )
  }
}

pub struct SbiConsole;

impl Write for SbiConsole {
  fn write_str(&mut self, s: &str) -> core::fmt::Result {
    for byte in s.bytes() {
      sbi_putchar(byte);
    }
    Ok(())
  }
}