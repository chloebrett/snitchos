#![no_std]
#![no_main]

use core::panic::PanicInfo;

core::arch::global_asm!(include_str!("entry.S"));

#[no_mangle]
pub extern "C" fn kmain(_hart_id: usize, _dtb_phys: usize) -> ! {
    park();
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    park();
}

fn park() -> ! {
    loop {
        unsafe { core::arch::asm!("wfi") }
    }
}
