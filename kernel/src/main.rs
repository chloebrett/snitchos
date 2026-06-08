#![no_std]
#![no_main]

extern crate alloc;

use core::arch::global_asm;
use core::sync::atomic::Ordering;
use fdt::Fdt;

mod console;
mod deflake;
mod demo_tasks;
mod dtb;
mod frame;
mod heap;
mod heap_smoke; // SMOKE TEST — remove once real workloads drive heap metrics
mod heartbeat;
mod ipi;
mod mmu;
mod panic;
mod percpu;
mod sbi;
mod secondary;
mod sched;
mod sync;
mod tracing;
mod trap;
mod uart;
mod virtio_console;
mod workload;

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
///
/// Single-byte raw-UART tracer. No locks, no formatting, no statics
/// touched (other than the MMIO read of `satp` to pick the address
/// space). Safe to call from anywhere in boot — including pre-MMU
/// (writes physical UART) and post-MMU (writes higher-half VA).
///
/// Temporary instrumentation to bisect heap-oom's silent early hang.
fn mark(c: u8) {
    unsafe {
        let satp: u64;
        core::arch::asm!("csrr {}, satp", out(reg) satp);
        let base = if satp != 0 {
            0x10000000usize + mmu::KERNEL_OFFSET
        } else {
            0x10000000usize
        };
        core::ptr::write_volatile(base as *mut u8, c);
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn kmain(_hart_id: usize, dtb_phys: usize) -> ! {
    mark(b'A');
    // DTB parse must come first — we need it to discover MMIO regions
    // before we build the boot page table. Pure parsing, no formatted
    // output, no fn-pointer-dispatched calls. Safe with MMU off
    // regardless of where the kernel is linked.
    let dtb = unsafe { Fdt::from_ptr(dtb_phys as *const u8) }.unwrap();
    mark(b'B');
    // MMIO regions: hardcoded for QEMU `virt`. DTB-driven discovery
    // (`collect_mmio_regions`) crashes pre-MMU under higher-half link
    // in a way we haven't isolated; see plans/v0.4-memory-findings.md.
    let mut mmio_regions = mmu::MmioRegions::new();
    mmio_regions.insert(mmu::QEMU_VIRT_MMIO_BASE);
    mark(b'C');

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
    mark(b'D');

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
    mark(b'E');

    // v0.6 step 4: install the per-hart pointer. `PER_HART_DATA` is a
    // higher-half static, so this must run post-trampoline.
    //
    // We pass *logical* hartid 0 here, regardless of what mhartid
    // OpenSBI handed us as `_hart_id`. The boot hart is by definition
    // logical hart 0 (kmain runs there once, owns all the boot
    // bookkeeping); the secondary always comes up as logical hart 1
    // via `secondary_main(_, 1)`. The platform `mhartid` is captured
    // separately into `BOOT_MHARTID` for telemetry; the dense logical
    // id is what every other piece of the kernel reasons about.
    //
    // SAFETY: trampoline executed (higher-half VAs resolve); no
    // per-hart-aware code has run yet.
    unsafe { percpu::init(0) };

    // Record the logical→mhartid mapping so `ipi::send(logical_id)`
    // can translate to the platform mhartid that `sbi_send_ipi`
    // expects. With MAX_HARTS=2 and OpenSBI free to pick either as
    // boot, the mapping is { 0 → _hart_id, 1 → 1-_hart_id }.
    percpu::LOGICAL_TO_MHARTID[0].store(_hart_id as u64, Ordering::Relaxed);
    percpu::LOGICAL_TO_MHARTID[1].store(1u64 - _hart_id as u64, Ordering::Relaxed);
    mark(b'F');

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
    mark(b'G');

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
    mark(b'H');

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

    // Register the metric set early — BEFORE timer init, spawns, or
    // anything else that might compete with us for CPU. Each
    // `register_*` call emits two virtio frames (StringRegister +
    // MetricRegister), so ~35 metrics ≈ 70 sends, each blocking on
    // the device. Doing this after task spawns would interleave the
    // sends with workload-task time slices, starving them. The
    // registered `Metrics` is held until `heartbeat::run` consumes
    // it at the end of kmain.
    let metrics = heartbeat::Metrics::register();

    // Arm the periodic timer and enable interrupts. From here on, the
    // CPU wakes us via timer IRQ instead of us spinning on the cycle
    // counter.
    //
    // SAFETY: trap vector was installed at the top of kmain; the
    // handler is ready.
    unsafe { trap::init_timer(timebase_hz) };

    // v0.6 step 7: enable S-mode software interrupts (the IPI
    // channel). Trap vector + handler are already installed; the
    // `ipi::handle_pending` dispatcher is the SSIP path.
    //
    // SAFETY: trap vector installed; sstatus.SIE already enabled
    // by init_timer above.
    unsafe { trap::enable_software_interrupts() };

    // Smoke: send ourselves a Wakeup IPI. The trap handler reads
    // ipi_pending via Acquire, bumps RECEIVED_TOTAL, and returns.
    // The `ipi-self-wakeup` integration scenario asserts the
    // counter reaches at least 1.
    ipi::send(percpu::current_hartid(), ipi::IPI_WAKEUP);

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

    // v0.5 step 5 smoke: build a marker task context, switch into
    // it, marker bumps a counter and switches back. Proves the
    // context-switch asm (`sched::switch`) round-trips correctly
    // before any of the real spawn/yield machinery is built on top.
    // If the asm is broken the kernel crashes or hangs here; the
    // `boot-reaches-heartbeat` scenario times out either way.
    //
    // SAFETY: called exactly once, with the heap live and no other
    // task running.
    unsafe { sched::smoke() };

    // v0.5 step 6: register the boot context as task 0 ("main") and
    // spawn a demo task. `register_bare_task` doesn't allocate a
    // stack — main inherits the boot stack. `spawn` allocates a
    // 16 KiB stack and rigs a `TaskContext` so the first switch
    // into the task lands in its entry function.
    //
    // The demo task sits on the runqueue idle until step 7's
    // `yield_now` picks it. `ThreadRegister` frames are emitted by
    // both calls so the collector can resolve task ids to names.
    let _ = sched::register_bare_task("main", kernel_core::sched::TaskState::Running);
    let _ = sched::spawn("idle", demo_tasks::idle_entry);
    // v0.5.x exit smoke: one task that bumps a counter then calls
    // `exit_now`. Asserts the asm + state-machine + snapshot-filter
    // wire together without crashing the kernel. Costs one 16 KiB
    // leaked stack at boot.
    let _ = sched::spawn("exit_smoke", sched::exit_smoke_entry);
    // Under `deflake-spawn-storm` the kernel boots with just main +
    // idle on hart 0 (and idle on hart 1, spawned in secondary_main).
    // The storm spawns its own minimal worker tasks on hart 1 below;
    // including the demo + workload tasks here would add unrelated
    // scheduling activity that masks the cross-hart race window.
    #[cfg(not(any(feature = "deflake-spawn-storm", feature = "deflake-ipi-pong", feature = "deflake-shootdown-storm")))]
    {
        let _ = sched::spawn("task_a", demo_tasks::task_a_entry);
        let _ = sched::spawn("task_b", demo_tasks::task_b_entry);
        let _ = sched::spawn("workload_producer", workload::producer_entry);
        let _ = sched::spawn("workload_consumer", workload::consumer_entry);
    }

    // DTB physical region lives in the identity gigapage we're about
    // to tear down. Drop the borrow first to make "no DTB access from
    // here on" load-bearing instead of incidental. `Fdt` has no `Drop`
    // impl, so this is just a binding-scope close.
    let _ = dtb;

    // v0.6 step 8: bring up the *other* hart. OpenSBI's choice of
    // boot hart isn't always 0 — under QEMU `-smp 2` it can hand us
    // `_hart_id=1`. So we compute the target as "any hart that isn't
    // me," which for MAX_HARTS=2 is just `1 - _hart_id`. We also
    // declare boot hart = hart whose mhartid is `_hart_id`, role Boot;
    // the SECONDARY slot of PER_HART_DATA still uses logical hart_id=1
    // because that's where we statically placed it.
    let boot_mhartid = _hart_id as u64;
    let secondary_mhartid = 1u64 - boot_mhartid;
    heartbeat::BOOT_MHARTID.store(boot_mhartid, Ordering::Relaxed);
    tracing::emit_hart_register(0, boot_mhartid, protocol::HartRole::Boot);
    unsafe { secondary::prepare_for_secondary() };
    let entry_pa = {
        unsafe extern "C" {
            fn _secondary_start();
        }
        mmu::va_to_pa(_secondary_start as *const () as usize) as u64
    };
    let err = sbi::hart_start(secondary_mhartid, entry_pa, 1);
    if err != 0 {
        panic!("sbi_hart_start({secondary_mhartid}) failed: error={err}");
    }
    // Acquire: pair with the Release on SECONDARY_READY in secondary_main.
    while !secondary::SECONDARY_READY.load(Ordering::Acquire) {
        core::hint::spin_loop();
    }
    // v0.6 step 10: cross-hart spawn smoke. Probe lands on hart 1's
    // runqueue + IPI wakeup nudges hart 1 to pick it. Gated out under
    // `deflake-spawn-storm` so hart 1 stays in `wfi` until the storm
    // loop's spawn_on's wake it — each wake then has the maximum
    // "fresh trap" race exposure.
    #[cfg(not(any(feature = "deflake-spawn-storm", feature = "deflake-ipi-pong", feature = "deflake-shootdown-storm")))]
    let _ = sched::spawn_on(1, "hart_1_probe", secondary::probe_entry);

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

    heartbeat::run(metrics)
}
