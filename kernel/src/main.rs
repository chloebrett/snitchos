#![no_std]
#![no_main]

use core::arch::{asm, global_asm};
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicBool, Ordering};
use fdt::Fdt;

mod console;
mod dtb;
mod mmu;
mod tracing;
mod trap;
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
    // DTB parse must come first — we need it to discover MMIO regions
    // before we build the boot page table. Pure parsing, no formatted
    // output, no fn-pointer-dispatched calls. Safe with MMU off
    // regardless of where the kernel is linked.
    let dtb = unsafe { Fdt::from_ptr(dtb_phys as *const u8) }.unwrap();
    // MMIO regions: hardcoded to [0x10000000, 0x10200000) — the 2 MiB
    // region containing the NS16550A UART and the virtio-mmio slots on
    // QEMU `virt`. DTB-driven discovery (`collect_mmio_regions`)
    // crashes pre-MMU under higher-half link in a way we haven't
    // isolated; see plans/v0.4-memory-findings.md.
    let mut mmio_regions = mmu::MmioRegions::new();
    mmio_regions.insert(0x10000000);

    // Turn paging on EARLY — before any code that loads an absolute
    // function-pointer value to a higher-half VA (formatted println!
    // via fmt::Arguments, trap_entry's address). After the linker
    // change to higher-half ORIGIN, those values point at higher-half
    // VAs that only resolve once the dual-map is live.
    //
    // span!() is safe pre-MMU because FrameSink dispatch was
    // monomorphized — no vtable, no fn pointers — and pre-init
    // buffering just copies bytes.
    //
    // SAFETY: MMU is off (boot default). Kernel image is dual-mapped
    // (identity + higher-half); MMIO regions identity-mapped.
    unsafe { mmu::enable(&mmio_regions, dtb_phys) };
    let timebase_hz = dtb::timebase_hz(&dtb)
        .expect("DTB missing /cpus/timebase-frequency — can't run without a clock") as u64;

    // Trap vector second. `trap_entry`'s address is now a higher-half
    // VA (linker change); writing it to stvec only makes sense with the
    // higher-half mapping live. Window between OpenSBI handoff and
    // here has no installed handler — we don't expect traps during
    // DTB parse / mmu::enable, same as the previous boot order.
    //
    // SAFETY: MMU on, higher-half mapped, trap path resolvable.
    unsafe { trap::set_trap_vector() };

    // Step 2a/2b smoke test: verify the higher-half mapping resolves.
    {
        span!("kernel.mmu.higher_half_verify");
        unsafe { mmu::verify_higher_half_mapping() };
    }

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
                    // Hello MUST be the first frame on the wire — the host
                    // anchors session wall-clock to its arrival. Then flush
                    // the spans we've been buffering so far (kernel.boot
                    // start, console_init pair, telemetry_init start).
                    tracing::send_hello(timebase_hz as u32);
                    tracing::flush_pre_init();
                    println!("virtio-console: ready");
                }
                Err(e) => println!("virtio-console: init failed: {:?}", e),
            }
        }
    }

    println!("I am alive — entering heartbeat");

    // Register the v0.2 metric set. `register_*` is idempotent; we
    // could call inside the loop, but pulling it out makes the
    // intent clearer and saves a per-iteration table lookup.
    let heartbeat_count = tracing::register_counter("snitchos.heartbeat.count");
    let intern_used = tracing::register_gauge("snitchos.intern.strings_used");
    let time_ticks = tracing::register_gauge("snitchos.time.ticks");
    let irq_duration = tracing::register_histogram("snitchos.irq.timer.duration_ticks");

    // Arm the periodic timer and enable interrupts. From here on, the
    // CPU wakes us via timer IRQ instead of us spinning on the cycle
    // counter.
    //
    // SAFETY: trap vector was installed at the top of kmain; the
    // handler is ready.
    unsafe { trap::init_timer(timebase_hz) };

    // Heartbeat loop: wfi until the timer IRQ flips TICK_PENDING,
    // then emit a span + the metric set.
    let mut count: i64 = 0;
    loop {
        while !trap::TICK_PENDING.swap(false, Ordering::Relaxed) {
            unsafe { asm!("wfi") };
        }
        {
            span!("kernel.heartbeat");
            count += 1;
            tracing::emit_metric(heartbeat_count, count);
            tracing::emit_metric(intern_used, tracing::intern_count() as i64);
            tracing::emit_metric(time_ticks, tracing::timestamp() as i64);
            // Histogram observation: how long the last IRQ took. The
            // handler measured rdtime delta; main thread emits.
            let dur = trap::LAST_IRQ_DURATION.load(Ordering::Relaxed);
            tracing::emit_metric(irq_duration, dur as i64);
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
        // SAFETY: bypassing the lock means we may interleave with whatever
        // was printing when the panic fired — accepted because we're already
        // in a fatal state.
        use core::fmt::Write;
        let mut uart = unsafe { uart::Uart16550::new(console::QEMU_VIRT_UART_BASE) };
        let _ = writeln!(&mut uart, "Kernel panic: {}", info);
    }
    loop {
        unsafe {
            asm!("wfi");
        }
    }
}
