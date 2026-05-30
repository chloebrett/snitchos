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
  la t0, __bss_start
  la t1, __bss_end
1: bgeu t0, t1, 2f
  sd zero, 0(t0)
  addi t0, t0, 8
  j 1b
2:
  call kmain
  # if kmain ever returns (it shouldn't), park
3: wfi
  j 3b
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
    loop {
      unsafe {
        asm!("wfi");
      }
    }
}
