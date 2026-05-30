#![no_std]
#![no_main]

use core::panic::PanicInfo;
use core::arch::global_asm;
use core::arch::asm;

global_asm!(include_str!("entry.S"));

#[unsafe(no_mangle)]
pub extern "C" fn kmain(_hart_id: usize, _dtb_phys: usize) -> ! {
  sbi_putchar(b'i');
  sbi_putchar(b' ');
  sbi_putchar(b'a');
  sbi_putchar(b'm');
  sbi_putchar(b' ');
  sbi_putchar(b'a');
  sbi_putchar(b'l');
  sbi_putchar(b'i');
  sbi_putchar(b'v');
  sbi_putchar(b'e');
  sbi_putchar(b'\n');
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
