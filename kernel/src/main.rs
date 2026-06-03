#![no_std]
#![no_main]

extern crate alloc;

use core::arch::{asm, global_asm};
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicBool, Ordering};
use fdt::Fdt;

mod console;
mod dtb;
mod frame;
mod heap;
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
/// Boot order is load-bearing — see comments inline. In particular,
/// `mmu::enable` must run before any code that touches an absolute
/// symbol VA (formatted `println!`, `set_trap_vector`), and
/// `va_to_pa` translation at every device-DMA site (in
/// `virtio_console`) must already be in place before the trampoline
/// moves PC to higher-half.
///
/// Known weaknesses:
/// - DTB lookups `.unwrap()` / `.expect()`. A malformed DTB panics
///   immediately; the panic handler may not produce output before
///   `console::init` runs.
#[unsafe(no_mangle)]
pub extern "C" fn kmain(_hart_id: usize, dtb_phys: usize) -> ! {
    // DTB parse must come first — we need it to discover MMIO regions
    // before we build the boot page table. Pure parsing, no formatted
    // output, no fn-pointer-dispatched calls. Safe with MMU off
    // regardless of where the kernel is linked.
    let dtb = unsafe { Fdt::from_ptr(dtb_phys as *const u8) }.unwrap();
    // MMIO regions: hardcoded for QEMU `virt`. DTB-driven discovery
    // (`collect_mmio_regions`) crashes pre-MMU under higher-half link
    // in a way we haven't isolated; see plans/v0.4-memory-findings.md.
    let mut mmio_regions = mmu::MmioRegions::new();
    mmio_regions.insert(mmu::QEMU_VIRT_MMIO_BASE);

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

    // Trampoline to higher-half: jump PC and shift sp by KERNEL_OFFSET.
    // The dual-map keeps identity addresses valid for `ra` values
    // already on the stack, but new function calls use PC-relative
    // addressing that now lands on higher-half. `&static as usize`
    // values produce higher-half VAs from here on; any address we
    // hand to a device must go through `mmu::va_to_pa` — see uses in
    // `virtio_console`.
    //
    // Inline (not a function) because `ret` from a trampoline fn
    // would jump back to the caller's identity-VA `ra`, defeating
    // the purpose.
    //
    // SAFETY: dual-map is live (`mmu::enable` above); sp's old and
    // new VAs alias the same physical stack page.
    unsafe {
        core::arch::asm!(
            "lla  t0, 1f",         // t0 = identity-PC VA of 1f
            "add  t0, t0, {off}",  // t0 = higher-half VA of 1f
            "add  sp, sp, {off}",  // sp = higher-half VA of stack top
            "jr   t0",
            "1:",
            off = in(reg) mmu::KERNEL_OFFSET,
            out("t0") _,
            options(nostack),
        );
    }

    // Verify we're actually at higher-half PC. `auipc rd, 0` puts
    // `current_pc + 0` in `rd`, so the result is the runtime address
    // of this instruction. If the trampoline silently no-ops in a
    // future regression, this comes back identity-range and the
    // `kernel.runs_at_higher_half` span never emits — caught by the
    // matching integration scenario.
    let pc: usize;
    unsafe { core::arch::asm!("auipc {}, 0", out(reg) pc) };
    if pc >= mmu::KERNEL_OFFSET {
        span!("kernel.runs_at_higher_half");
    }

    let timebase_hz = dtb::timebase_hz(&dtb)
        .expect("DTB missing /cpus/timebase-frequency — can't run without a clock") as u64;

    // Install the trap vector. `trap_entry`'s symbol value is a
    // higher-half VA, so `stvec` only points somewhere meaningful with
    // the MMU on. The window between OpenSBI handoff and here has no
    // installed handler — we don't expect traps during DTB parse,
    // `mmu::enable`, or the trampoline.
    //
    // SAFETY: MMU on, higher-half mapped, trap path resolvable.
    unsafe { trap::set_trap_vector() };

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
    let frames_allocated = tracing::register_counter("snitchos.frames.allocated_total");
    let frames_freed = tracing::register_counter("snitchos.frames.freed_total");
    let frames_alloc_failed = tracing::register_counter("snitchos.frames.alloc_failed_total");
    let frames_in_use = tracing::register_gauge("snitchos.frames.in_use");
    let frames_free = tracing::register_gauge("snitchos.frames.free");
    let heap_alloc_total = tracing::register_counter("snitchos.heap.alloc_total");
    let heap_dealloc_total = tracing::register_counter("snitchos.heap.dealloc_total");
    let heap_alloc_failed = tracing::register_counter("snitchos.heap.alloc_failed_total");
    let heap_bytes_used = tracing::register_gauge("snitchos.heap.bytes_used");

    // Arm the periodic timer and enable interrupts. From here on, the
    // CPU wakes us via timer IRQ instead of us spinning on the cycle
    // counter.
    //
    // SAFETY: trap vector was installed at the top of kmain; the
    // handler is ready.
    unsafe { trap::init_timer(timebase_hz) };

    // Frame allocator init. Walks the DTB's `/memory` node, marks
    // SBI / kernel-image / DTB regions as reserved, releases everything
    // else into the free pool. After this returns, `frame::alloc()` is
    // usable from anywhere.
    //
    // Must run before `unmap_identity` because the DTB region lives in
    // the identity kernel gigapage that's about to disappear.
    //
    // SAFETY: called exactly once, with a valid `&Fdt` and the
    // corresponding `dtb_phys`, post-trampoline (so `__kernel_*`
    // symbol VAs resolve and `va_to_pa` recovers their physical bounds).
    unsafe { frame::init_from_dtb(&dtb, dtb_phys).expect("frame allocator init") };

    // Kernel heap. Pulls a contiguous run of frames from the frame
    // allocator and hands their linear-map VA to
    // `linked_list_allocator`. After this, anything in `alloc::`
    // (`Box`, `Vec`, formatted strings that exceed the stack buffer)
    // works inside the kernel.
    //
    // SAFETY: called exactly once, after frame allocator init, with
    // the linear map live (installed by `mmu::enable`).
    unsafe { heap::init() };

    // DTB physical region lives in the identity gigapage we're about
    // to tear down. Drop the borrow first to make "no DTB access from
    // here on" load-bearing instead of incidental. `Fdt` has no `Drop`
    // impl, so this is just a binding-scope close.
    let _ = dtb;

    // Tear down both identity mappings. From here on, any access to
    // an identity-half VA — kernel image, stack, DTB, or MMIO — faults.
    // The kernel runs purely in higher-half: code, statics, `CONSOLE`,
    // `UART`, and the emergency UART path (via `emergency_uart_base`)
    // all hold or compute higher-half VAs.
    //
    // SAFETY: kernel is running at higher-half PC + sp (trampoline
    // executed above). `CONSOLE` and `UART` were initialized with
    // higher-half VAs in earlier increments.
    unsafe { mmu::unmap_identity() };

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
            // Smoke pattern that exercises the allocator each heartbeat.
            // Default build: alloc+free, keeps `in_use` bounded.
            // `oom-leak` feature: leak 8192 frames per tick (32 MiB)
            // so the allocator's ~32K-frame free pool exhausts in
            // ~4 heartbeats. Used by the `frame-allocator-oom`
            // integration scenario.
            #[cfg(not(feature = "oom-leak"))]
            {
                if let Some(frame) = frame::alloc_zeroed() {
                    frame::free(frame);
                }
            }
            #[cfg(feature = "oom-leak")]
            {
                for _ in 0..8192 {
                    let _ = frame::alloc_zeroed();
                }
            }
            // Heap smoke: allocate, write, drop. Proves the heap is
            // live and produces movement on the heap metrics so the
            // dashboards aren't flat zeros. Cheap (256 B), runs every
            // heartbeat.
            {
                let mut v: alloc::vec::Vec<u8> = alloc::vec::Vec::with_capacity(256);
                v.push(count as u8);
            }
            tracing::emit_metric(heartbeat_count, count);
            tracing::emit_metric(intern_used, tracing::intern_count() as i64);
            tracing::emit_metric(time_ticks, tracing::timestamp() as i64);
            // Histogram observation: how long the last IRQ took. The
            // handler measured rdtime delta; main thread emits.
            let dur = trap::LAST_IRQ_DURATION.load(Ordering::Relaxed);
            tracing::emit_metric(irq_duration, dur as i64);
            // Frame allocator telemetry. Counters drain atomically;
            // gauges briefly take the allocator lock (heartbeat is
            // single-threaded so no contention).
            tracing::emit_metric(
                frames_allocated,
                frame::ALLOC_COUNT.load(Ordering::Relaxed) as i64,
            );
            tracing::emit_metric(
                frames_freed,
                frame::FREE_COUNT.load(Ordering::Relaxed) as i64,
            );
            tracing::emit_metric(
                frames_alloc_failed,
                frame::ALLOC_FAIL_COUNT.load(Ordering::Relaxed) as i64,
            );
            if let Some(stats) = frame::stats() {
                tracing::emit_metric(frames_in_use, stats.in_use as i64);
                tracing::emit_metric(frames_free, stats.free as i64);
            }
            // Kernel heap telemetry. All atomics — no allocator lock
            // is held during emission. `bytes_used` is approximate
            // (sum of `layout.size()`, not the heap's internal
            // bookkeeping including per-block overhead) but the
            // shape is what Grafana needs.
            tracing::emit_metric(
                heap_alloc_total,
                heap::ALLOC_COUNT.load(Ordering::Relaxed) as i64,
            );
            tracing::emit_metric(
                heap_dealloc_total,
                heap::DEALLOC_COUNT.load(Ordering::Relaxed) as i64,
            );
            tracing::emit_metric(
                heap_alloc_failed,
                heap::ALLOC_FAIL_COUNT.load(Ordering::Relaxed) as i64,
            );
            tracing::emit_metric(
                heap_bytes_used,
                heap::BYTES_USED.load(Ordering::Relaxed) as i64,
            );
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
        // in a fatal state. `emergency_uart_base` reads satp so this works
        // in any boot stage (MMU off, identity-MMIO mapped, or higher-half-only).
        use core::fmt::Write;
        let mut uart = unsafe { uart::Uart16550::new(console::emergency_uart_base()) };
        let _ = writeln!(&mut uart, "Kernel panic: {}", info);
    }
    loop {
        unsafe {
            asm!("wfi");
        }
    }
}
