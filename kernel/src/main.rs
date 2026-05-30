#![no_std]
#![no_main]

use core::panic::PanicInfo;
use core::arch::global_asm;
use core::arch::asm;

global_asm!(r#"
.section .text.boot
.globl _start
_start:
  la sp, __stack_top
  call kmain
  # if kmain ever returns (it shouldn't), park
1: wfi
  j 1b
"#);

#[unsafe(no_mangle)]
pub extern "C" fn kmain(_hart_id: usize, _dtb_phys: usize) -> ! {
    loop {
      unsafe {
        asm!("wfi");
      }
    }
}

/// This function is called on panic.
#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {}
}
