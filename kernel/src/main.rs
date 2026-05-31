#![no_std]
#![no_main]

use core::arch::{asm, global_asm};
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicBool, Ordering};
use fdt::Fdt;

mod console;
mod dtb;
mod tracing;
mod uart;
mod virtio_console;

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
    // DTB parse runs before the kernel.boot span: the parse needs to
    // succeed before we even know where the UART is, and tracing isn't
    // useful before there's a way to emit. Treat it as bootstrap.
    let dtb = unsafe { Fdt::from_ptr(dtb_phys as *const u8) }.unwrap();
    let timebase_hz = dtb::timebase_hz(&dtb) as u64;

    // Open kernel.boot, with sub-spans for each init phase. All frames
    // emitted before virtio-console is ready get pre-init-buffered.
    {
        span!("kernel.boot");

        let uart_addr = dtb::uart_addr(&dtb);
        {
            span!("console_init");
            // SAFETY: uart_addr came from the DTB's ns16550a node.
            unsafe { console::init(uart_addr) };
        }

        dtb::print_info(&dtb, uart_addr);

        {
            span!("telemetry_init");
            // SAFETY: dtb came from the DTB we just parsed.
            match unsafe { virtio_console::init(&dtb) } {
                Ok(()) => {
                    // Flush the spans we've buffered so far (kernel.boot
                    // start, console_init pair, telemetry_init start).
                    tracing::flush_pre_init();
                    println!("virtio-console: ready");
                    send_hello(timebase_hz as u32);
                }
                Err(e) => println!("virtio-console: init failed: {:?}", e),
            }
        }
    }

    println!("I am alive — entering heartbeat");

    // Heartbeat loop: emit a span once per timebase tick (1 second on QEMU).
    let mut next = tracing::timestamp() + timebase_hz;
    loop {
        while tracing::timestamp() < next {}
        {
            span!("kernel.heartbeat");
        }
        next += timebase_hz;
    }
}

/// Encode a `Frame::Hello` with the discovered CPU timebase and ship it
/// out the virtio-console. First real telemetry on the wire.
fn send_hello(timebase_hz: u32) {
    let frame = protocol::Frame::Hello {
        timebase_hz: timebase_hz as u64,
        protocol_version: 1,
    };
    let mut buf = [0u8; 32];
    match postcard::to_slice(&frame, &mut buf) {
        Ok(encoded) => {
            virtio_console::send(encoded);
            println!("sent Hello ({} bytes)", encoded.len());
        }
        Err(e) => println!("postcard encode failed: {:?}", e),
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
