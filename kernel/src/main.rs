#![no_std]
#![no_main]

extern crate alloc;

use core::arch::{asm, global_asm};
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use fdt::Fdt;

mod console;
mod dtb;
mod frame;
mod heap;
mod heap_smoke; // SMOKE TEST — remove once real workloads drive heap metrics
mod ipi;
mod mmu;
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

#[cfg(feature = "deflake-shootdown-storm")]
mod deflake_shootdown {
    //! Tight `mmu::shootdown(va)` loop from hart 0. Each iteration:
    //!
    //!   - hart 0 writes `shootdown_va` into hart 1's PerHartData,
    //!     snapshots `shootdown_ack`, sends `IPI_TLB_SHOOTDOWN`,
    //!     spin-waits (Acquire) on `shootdown_ack` to advance.
    //!   - hart 1 takes the IPI; `ipi::handle_pending` does
    //!     `ipi_pending.swap(0, Acquire)`, reads `shootdown_va`,
    //!     runs `sfence.vma`, bumps `shootdown_ack` with Release.
    //!
    //! Tests the IPI payload-read path — distinct from `deflake-ipi-pong`
    //! which has no payload. The race window we're probing: does
    //! hart 1 actually see the `shootdown_va` value hart 0 wrote
    //! before the IPI, on multi-thread TCG? If not, hart 1 sfences
    //! the wrong address; the test still completes (sfence with any
    //! VA is harmless on a fresh-mapping kernel) but the
    //! `shootdowns_received_total` counter still climbs.
    //!
    //! **Built-in confounder.** Hart 0's spin-wait inside
    //! `mmu::shootdown` is itself an Acquire load on
    //! `shootdown_ack`. If multi-thread TCG drops it, hart 0 wedges
    //! before hart 1 can be blamed. We accept this trade — the
    //! kernel's existing API is the API we're testing. A wedge here
    //! is informative either way (kernel does have a real bug;
    //! whether on hart 0 or hart 1 is downstream of the wedge).
    //!
    //! Choice of VA: `KERNEL_OFFSET` — a real higher-half VA that
    //! is always mapped, so sfence on it on hart 1 is a real TLB op,
    //! not a no-op. Using an arbitrary unmapped VA would sfence
    //! nothing; using a userspace VA (none exist) would be
    //! meaningless. KERNEL_OFFSET is also the address whose mapping
    //! v0.4 step 2 set up, so it's the canonical "real translation"
    //! to flush.

    use core::sync::atomic::{AtomicU64, Ordering};

    pub const N: u64 = 5_000;
    const DELAY_TICKS: u64 = 2_000;

    /// Count of shootdowns initiated by hart 0. The heartbeat re-emits
    /// as `snitchos.deflake.shootdown_storm_sends`. `Relaxed` —
    /// single writer.
    pub static SENDS: AtomicU64 = AtomicU64::new(0);

    fn now() -> u64 {
        let t: u64;
        // SAFETY: rdtime is S-mode-readable; no side effects.
        unsafe { core::arch::asm!("rdtime {}", out(reg) t) };
        t
    }

    pub fn run() {
        let va = crate::mmu::KERNEL_OFFSET;
        for _ in 0..N {
            crate::mmu::shootdown(va);
            SENDS.fetch_add(1, Ordering::Relaxed);
            let deadline = now() + DELAY_TICKS;
            while now() < deadline {
                core::hint::spin_loop();
            }
        }
    }
}

#[cfg(feature = "deflake-ipi-pong")]
mod deflake_ipi_pong {
    //! Tight IPI_WAKEUP loop from hart 0 to hart 1. Each iteration is
    //! one `hart 1 in wfi → IPI → trap → swap-Acquire → sret → resume`
    //! trial — directly the post-sret memory-ordering window the
    //! deflake residual was suspected to live on. No spawning, no
    //! heap growth: scales to ~10k trials per boot.
    //!
    //! Pacing: hart 0 sends one IPI, then spins on `rdtime` for
    //! `DELAY_TICKS` before the next send. At timebase 10 MHz, 1 000
    //! ticks ≈ 100 µs — long enough that hart 1 (`hart_1_main`'s
    //! `yield → wfi → wake → yield`) re-enters `wfi` before the next
    //! IPI lands. Without pacing the IPIs coalesce into one trap on
    //! hart 1 and the trial count collapses.
    //!
    //! No MMIO touches on hart 0 inside the loop — the storm's whole
    //! point is to leave the kernel's race window unfenced. `rdtime`
    //! is a CSR read, not MMIO.

    use core::sync::atomic::{AtomicU64, Ordering};

    pub const N: u64 = 10_000;

    /// Inter-IPI delay on hart 0, in `rdtime` ticks. Tuned so hart 1
    /// has time to `sret → loop iter → yield → wfi` before the next
    /// IPI lands. Too low: IPIs coalesce (RECEIVED_TOTAL << N). Too
    /// high: storm takes forever.
    const DELAY_TICKS: u64 = 1_000;

    /// Count of IPI sends issued by hart 0. Bumped after each send;
    /// the heartbeat re-emits as `snitchos.deflake.ipi_pong_sends`.
    /// Survival signal: if hart 0 wedged or the kernel hung mid-loop,
    /// this counter stays below N. `Relaxed` — single writer.
    pub static SENDS: AtomicU64 = AtomicU64::new(0);

    fn now() -> u64 {
        let t: u64;
        // SAFETY: rdtime is a S-mode-readable CSR; reading it has no
        // side effects.
        unsafe { core::arch::asm!("rdtime {}", out(reg) t) };
        t
    }

    /// Drive the storm. Sends N IPIs paced by DELAY_TICKS each.
    pub fn run() {
        for _ in 0..N {
            crate::ipi::send(1, crate::ipi::IPI_WAKEUP);
            SENDS.fetch_add(1, Ordering::Relaxed);
            let deadline = now() + DELAY_TICKS;
            while now() < deadline {
                core::hint::spin_loop();
            }
        }
    }
}

#[cfg(feature = "deflake-spawn-storm")]
mod deflake_storm {
    //! Cross-hart spawn storm for the `deflake-spawn-storm` integration
    //! scenario. Drives N serialised `spawn_on(1, body)` iterations from
    //! hart 0 with an MMIO-fenced ack wait. Each iteration is one trial
    //! of the residual cross-hart memory-ordering race on hart 1's
    //! IPI pickup path. See `plans/residual-race-investigation.md`.

    use core::sync::atomic::{AtomicU64, Ordering};

    /// Number of spawn iterations. With per-trial flake rate ≈ 1%
    /// (eyeballed from suite-wide 8% across ~8 cross-hart trials), 200
    /// iterations gives ~87% per-run flake without the fix — enough
    /// signal for `--repeat 5`.
    pub const N: u64 = 200;

    /// Cumulative count of spawned tasks that reached their body and
    /// bumped this counter. Hart 0's storm loop polls it after each
    /// spawn; the heartbeat re-emits its value every tick as the
    /// `snitchos.deflake.spawn_storm_acks` metric.
    /// `Release` on the bump pairs with `Acquire` on hart 0's poll —
    /// but multi-thread TCG is the entire reason we don't trust that
    /// pairing, hence `fence_via_uart_lsr` below.
    pub static ACK_COUNTER: AtomicU64 = AtomicU64::new(0);

    /// Body run by every storm task on hart 1. Touches a stack-local
    /// (H2 probe — proves the new task's `sp` resolves to writable
    /// memory), bumps the ack counter, then cycles through `yield_now`
    /// forever because v0.5 tasks can't exit.
    pub extern "C" fn body() -> ! {
        let marker: u64 = 0xdead_beef_cafe_f00d;
        core::hint::black_box(marker);
        ACK_COUNTER.fetch_add(1, Ordering::Release);
        loop {
            crate::sched::yield_now();
        }
    }

    /// Single-MMIO-read BQL fence used by hart 0's ack-wait spin. The
    /// UART LSR at base+5 has no side effects on read. QEMU's MMIO
    /// path acquires the Big QEMU Lock, which serialises against
    /// other vCPUs and incidentally provides the cross-hart memory
    /// fence that multi-thread TCG drops on plain `Acquire`. Hart 0
    /// is the observer, not the hart under test — fencing it does
    /// not interfere with the race window on hart 1's pickup path.
    fn fence_via_uart_lsr() {
        let lsr = crate::console::emergency_uart_base() + 5;
        // SAFETY: lsr is the 16550 line-status register; reading it is
        // a non-destructive observation. The address is mapped (the
        // emergency UART base is always reachable post-MMU).
        unsafe { core::ptr::read_volatile(lsr as *const u8) };
    }

    /// Drive the storm. Iteration `i` spawns one minimal task on
    /// hart 1 and waits for the ack counter to exceed `i` before
    /// advancing. Returns when all N tasks have acked.
    pub fn run() {
        for i in 0..N {
            crate::sched::spawn_on(1, "deflake", body);
            loop {
                fence_via_uart_lsr();
                if ACK_COUNTER.load(Ordering::Acquire) > i {
                    break;
                }
                core::hint::spin_loop();
            }
        }
    }
}

// Pull in the boot stub (entry.S). It defines `_start`, sets up the stack
// pointer, zeros .bss, and calls `kmain`. See linker.ld for the memory layout
// it depends on (__stack_top, __bss_start, __bss_end).
global_asm!(include_str!("entry.S"));

/// DEFLAKE: lock-free single-line UART tag. Used to bisect where the kernel
/// dies during the Bug B silent reset. SAFETY: bypasses CONSOLE/UART mutex,
/// so output may interleave with concurrent prints — accepted for forensic
/// instrumentation. `emergency_uart_base` reads satp so this works in any
/// boot stage.
pub(crate) fn tag(s: &str) {
    use core::fmt::Write;
    let mut uart = unsafe { uart::Uart16550::new(console::emergency_uart_base()) };
    let _ = writeln!(&mut uart, "[TAG] {s}");
}

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
    let heap_bytes_capacity = tracing::register_gauge("snitchos.heap.bytes_capacity");
    let heap_bytes_used = tracing::register_gauge("snitchos.heap.bytes_used");
    let heap_bytes_free = tracing::register_gauge("snitchos.heap.bytes_free");
    let heap_grow_total = tracing::register_counter("snitchos.heap.grow_total");
    let heap_grow_failed = tracing::register_counter("snitchos.heap.grow_failed_total");
    let heap_free_blocks = tracing::register_gauge("snitchos.heap.free_blocks");
    let heap_largest_free_block = tracing::register_gauge("snitchos.heap.largest_free_block_bytes");
    let sched_smoke_marker_hits = tracing::register_counter("snitchos.sched.smoke_marker_hits");
    let sched_context_switches = tracing::register_counter("snitchos.sched.context_switches_total");
    let sched_runqueue_depth = tracing::register_gauge("snitchos.sched.runqueue_depth");
    let sched_tasks_total = tracing::register_gauge("snitchos.sched.tasks_total");
    let sched_yield_overhead = tracing::register_histogram("snitchos.sched.yield_overhead_ticks");
    let task_a_loops = tracing::register_counter("snitchos.task_a.loops");
    let task_b_loops = tracing::register_counter("snitchos.task_b.loops");
    let workload_produced = tracing::register_counter("snitchos.workload.samples_produced_total");
    let workload_consumed = tracing::register_counter("snitchos.workload.samples_consumed_total");
    let workload_histogram_sum = tracing::register_gauge("snitchos.workload.histogram_sum");
    let workload_lock_wait = tracing::register_counter("snitchos.workload.lock_wait_ticks_total");
    let workload_queue_depth = tracing::register_gauge("snitchos.workload.queue_depth");
    let ipi_received = tracing::register_counter("snitchos.ipi.received_total");
    // v0.6 step 8 SMP visibility metrics. `harts_total` is the build-
    // time hart-count cap; `boot_hart_id` is what mhartid OpenSBI gave
    // us as the boot hart (not always 0 under -smp 2); `secondary_wfi`
    // ticks each time the secondary hart wakes from wfi (today only
    // step 10's runqueue + IPI wakeup will give it anything to do —
    // before that it climbs on stray interrupts only).
    let smp_harts_total = tracing::register_gauge("snitchos.smp.harts_total");
    let smp_boot_hart_id = tracing::register_gauge("snitchos.smp.boot_hart_id");
    let smp_secondary_wfi = tracing::register_counter("snitchos.smp.secondary_wfi_total");
    let mmu_shootdowns_received =
        tracing::register_counter("snitchos.mmu.shootdowns_received_total");
    let mmu_shootdowns_sent =
        tracing::register_counter("snitchos.mmu.shootdowns_sent_total");
    let smp_probe_ticks =
        tracing::register_counter("snitchos.smp.hart_1_probe_ticks_total");
    // Spawn-storm scenario completion metric. Emitted every heartbeat
    // under the feature, monotonically tracking how many storm tasks
    // have acked. The scenario asserts it reaches deflake_storm::N.
    #[cfg(feature = "deflake-spawn-storm")]
    let spawn_storm_acks = tracing::register_counter("snitchos.deflake.spawn_storm_acks");
    // IPI-pong scenario completion metric. Emitted every heartbeat
    // under the feature; reaches `N` when hart 0 has finished
    // sending all IPIs. The scenario asserts both this value and
    // `snitchos.ipi.received_total` so we can tell apart "hart 0
    // wedged" (sends stays low) from "hart 1 stopped receiving"
    // (sends reaches N but received does not).
    #[cfg(feature = "deflake-ipi-pong")]
    let ipi_pong_sends = tracing::register_counter("snitchos.deflake.ipi_pong_sends");
    // Shootdown storm completion metric. Reaches `N` when hart 0 has
    // finished initiating all shootdowns (each blocks until hart 1's
    // ack returns). If hart 0 wedges on a Acquire-load of
    // shootdown_ack (built-in confounder) or hart 1 wedges on its
    // pickup, this stays below N.
    #[cfg(feature = "deflake-shootdown-storm")]
    let shootdown_storm_sends =
        tracing::register_counter("snitchos.deflake.shootdown_storm_sends");
    // SMOKE TEST metrics — remove with heap_smoke module
    let smoke_entries = tracing::register_gauge("snitchos.heap_smoke.entries");
    let smoke_primes = tracing::register_gauge("snitchos.heap_smoke.primes");
    let smoke_candidate = tracing::register_gauge("snitchos.heap_smoke.candidate");

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
    let _ = sched::spawn("idle", idle_entry);
    // Under `deflake-spawn-storm` the kernel boots with just main +
    // idle on hart 0 (and idle on hart 1, spawned in secondary_main).
    // The storm spawns its own minimal worker tasks on hart 1 below;
    // including the demo + workload tasks here would add unrelated
    // scheduling activity that masks the cross-hart race window.
    #[cfg(not(any(feature = "deflake-spawn-storm", feature = "deflake-ipi-pong", feature = "deflake-shootdown-storm")))]
    {
        let _ = sched::spawn("task_a", task_a_entry);
        let _ = sched::spawn("task_b", task_b_entry);
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
    BOOT_MHARTID.store(boot_mhartid, Ordering::Relaxed);
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

    // Heartbeat loop: wfi until the timer IRQ flips TICK_PENDING,
    // then emit a span + the metric set.
    let mut count: i64 = 0;
    loop {
        // Main as task 0: check for a pending tick; if set, do the
        // heartbeat work; either way, yield. The `wfi` for "nothing
        // to do, just wait" lives in the idle thread now — main
        // doesn't sleep, it just rounds through the scheduler.
        if !trap::TICK_PENDING.this_cpu().swap(false, Ordering::Relaxed) {
            sched::yield_now();
            continue;
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
            // Heap smoke. Default build: alloc + write + drop a 256 B
            // Vec — proves the heap is live, keeps `bytes_used` near
            // 0 across heartbeats.
            //
            // `heap-oom` feature: per-heartbeat leak loop using the
            // raw GlobalAlloc API (returns null on failure rather than
            // panicking through `alloc_error_handler`). After the
            // heap exhausts, every subsequent iteration's first
            // allocation returns null immediately and bumps
            // `alloc_failed_total` — so the counter climbs once per
            // heartbeat post-OOM, and the kernel keeps heartbeating.
            #[cfg(not(feature = "heap-oom"))]
            {
                let mut v: alloc::vec::Vec<u8> = alloc::vec::Vec::with_capacity(256);
                v.push(count as u8);
            }
            #[cfg(feature = "heap-oom")]
            {
                // Leak 4096 × 4 KiB blocks per heartbeat (16 MiB/tick).
                // P2's watermark grow adds 1 MiB/tick, so net pressure
                // is +15 MiB/tick — the ~120 MiB usable RAM (post
                // kernel + bitmap + tables) exhausts in ~8 heartbeats.
                // `try_reserve_exact` returns `Err` rather than
                // panicking; the underlying null-return from
                // `GlobalAlloc::alloc` still bumps `ALLOC_FAIL_COUNT`
                // so the OOM signal makes it to telemetry.
                for _ in 0..4096 {
                    let mut v: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
                    if v.try_reserve_exact(4096).is_err() {
                        break;
                    }
                    core::mem::forget(v);
                }
            }
            tracing::emit_metric(heartbeat_count, count);
            tracing::emit_metric(intern_used, tracing::intern_count() as i64);
            tracing::emit_metric(time_ticks, tracing::timestamp() as i64);
            // Histogram observation: how long the last IRQ took. The
            // handler measured rdtime delta; main thread emits.
            let dur = trap::LAST_IRQ_DURATION.this_cpu().load(Ordering::Relaxed);
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
            // Kernel heap telemetry. Counters are atomics; the byte
            // gauges come from `heap::stats()`, which briefly takes
            // the heap lock — safe from the heartbeat (single-
            // threaded, no contention with allocator callers).
            // `bytes_used` sums alignment-padded `layout.size()` for
            // live allocations; it excludes hole-list metadata, so
            // it slightly undercounts unavailable bytes.
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
            if let Some(hstats) = heap::stats() {
                tracing::emit_metric(heap_bytes_capacity, hstats.capacity as i64);
                tracing::emit_metric(heap_bytes_used, hstats.used as i64);
                tracing::emit_metric(heap_bytes_free, hstats.free as i64);
                tracing::emit_metric(heap_free_blocks, hstats.free_blocks as i64);
                tracing::emit_metric(heap_largest_free_block, hstats.largest_free_block as i64);
                // Watermark grow. The policy (when + by how much) is
                // pure logic in `kernel_core::heap`; this loop owns
                // the side effect of acting on the decision.
                // Heartbeat is single-threaded so it's safe to take
                // the heap lock for `extend`. On failure (ceiling,
                // OOM, map error) we bump `grow_failed_total` and
                // keep going — the next alloc fails with
                // `alloc_failed_total` as today.
                if let Some(frames) =
                    heap::watermark_grow_decision(hstats, &heap::WATERMARK)
                {
                    let _ = heap::extend(frames);
                }
            }
            tracing::emit_metric(
                heap_grow_total,
                heap::GROW_COUNT.load(Ordering::Relaxed) as i64,
            );
            tracing::emit_metric(
                heap_grow_failed,
                heap::GROW_FAIL_COUNT.load(Ordering::Relaxed) as i64,
            );
            tracing::emit_metric(
                sched_smoke_marker_hits,
                sched::SMOKE_MARKER_HITS.load(Ordering::Relaxed) as i64,
            );
            tracing::emit_metric(
                sched_context_switches,
                sched::CONTEXT_SWITCHES.load(Ordering::Relaxed) as i64,
            );
            let sched_snap = sched::stats();
            tracing::emit_metric(sched_runqueue_depth, sched_snap.runqueue_depth as i64);
            tracing::emit_metric(sched_tasks_total, sched_snap.tasks_total as i64);
            tracing::emit_metric(
                sched_yield_overhead,
                sched::LAST_YIELD_OVERHEAD_TICKS.load(Ordering::Relaxed) as i64,
            );
            // Per-task metrics: gated off under `deflake-spawn-storm`
            // because that build uses sentinel StringIds for these
            // (see Task::new_bare) — emitting against id 0 would
            // mis-tag whichever name id 0 is.
            #[cfg(not(feature = "deflake-spawn-storm"))]
            for snap in sched::task_snapshots() {
                tracing::emit_metric(snap.cpu_time_metric, snap.cpu_time_ticks as i64);
                tracing::emit_metric(snap.runs_metric, snap.runs as i64);
            }
            tracing::emit_metric(
                task_a_loops,
                TASK_A_LOOPS.load(Ordering::Relaxed) as i64,
            );
            tracing::emit_metric(
                task_b_loops,
                TASK_B_LOOPS.load(Ordering::Relaxed) as i64,
            );
            // v0.6 step 1: producer/consumer workload.
            tracing::emit_metric(
                workload_produced,
                workload::SAMPLES_PRODUCED.load(Ordering::Relaxed) as i64,
            );
            tracing::emit_metric(
                workload_consumed,
                workload::SAMPLES_CONSUMED.load(Ordering::Relaxed) as i64,
            );
            tracing::emit_metric(
                workload_histogram_sum,
                workload::histogram_sum() as i64,
            );
            tracing::emit_metric(
                workload_lock_wait,
                workload::LOCK_WAIT_TICKS_TOTAL.load(Ordering::Relaxed) as i64,
            );
            tracing::emit_metric(
                workload_queue_depth,
                workload::queue_depth() as i64,
            );
            tracing::emit_metric(
                ipi_received,
                ipi::RECEIVED_TOTAL.load(Ordering::Relaxed) as i64,
            );
            tracing::emit_metric(smp_harts_total, percpu::MAX_HARTS as i64);
            tracing::emit_metric(
                smp_boot_hart_id,
                BOOT_MHARTID.load(Ordering::Relaxed) as i64,
            );
            tracing::emit_metric(
                smp_secondary_wfi,
                secondary::SECONDARY_WFI_COUNT.load(Ordering::Relaxed) as i64,
            );
            tracing::emit_metric(
                mmu_shootdowns_received,
                ipi::SHOOTDOWNS_RECEIVED_TOTAL.load(Ordering::Relaxed) as i64,
            );
            tracing::emit_metric(
                mmu_shootdowns_sent,
                mmu::SHOOTDOWNS_SENT_TOTAL.load(Ordering::Relaxed) as i64,
            );
            tracing::emit_metric(
                smp_probe_ticks,
                secondary::PROBE_TICKS.load(Ordering::Relaxed) as i64,
            );
            // SMOKE TEST — remove with heap_smoke module
            heap_smoke::step(count);
            let sst = heap_smoke::stats();
            tracing::emit_metric(smoke_entries, sst.entries as i64);
            tracing::emit_metric(smoke_primes, sst.primes as i64);
            tracing::emit_metric(smoke_candidate, sst.candidate as i64);
            // Spawn-storm: run once on the first heartbeat tick. Blocks
            // main until all N tasks ack, so subsequent heartbeats see
            // the storm already complete. Storm completion is observed
            // by the integration scenario via the metric below.
            #[cfg(feature = "deflake-spawn-storm")]
            {
                if count == 1 {
                    deflake_storm::run();
                }
                tracing::emit_metric(
                    spawn_storm_acks,
                    deflake_storm::ACK_COUNTER.load(Ordering::Relaxed) as i64,
                );
            }
            #[cfg(feature = "deflake-ipi-pong")]
            {
                if count == 1 {
                    deflake_ipi_pong::run();
                }
                tracing::emit_metric(
                    ipi_pong_sends,
                    deflake_ipi_pong::SENDS.load(Ordering::Relaxed) as i64,
                );
            }
            #[cfg(feature = "deflake-shootdown-storm")]
            {
                if count == 1 {
                    deflake_shootdown::run();
                }
                tracing::emit_metric(
                    shootdown_storm_sends,
                    deflake_shootdown::SENDS.load(Ordering::Relaxed) as i64,
                );
            }
        }
        sched::yield_now();
    }
}

/// Idle thread. The "what runs when nothing else wants the CPU"
/// task. `wfi` sleeps until any interrupt arrives (timer being the
/// only one v0.5 cares about); the subsequent `yield_now` hands
/// control to whoever is now ready.
extern "C" fn idle_entry() -> ! {
    loop {
        unsafe { asm!("wfi") };
        sched::yield_now();
    }
}

/// Demo tasks. Each opens a per-iteration `task_x.tick` span, bumps
/// its counter, and yields. With main and idle in the mix the
/// scheduler round-robins through all four; both tasks' `tick`
/// spans interleave on the wire, each correctly tagged with its
/// own `task_id`.
///
/// The span is opened-then-immediately-closed inside an explicit
/// block so it's never alive across the yield. Per-task `SpanCursor`
/// swapping on context switch is a future refinement; until then,
/// the discipline is "balance the cursor before yielding."
/// mhartid OpenSBI handed kmain as `_hart_id`. Captured at the top of
/// the bring-up block; drained by the heartbeat as
/// `snitchos.smp.boot_hart_id`. Useful because OpenSBI's choice of
/// boot hart is not always 0 under `-smp 2`. `Relaxed`: single
/// writer (boot path), read by heartbeat on the same hart.
static BOOT_MHARTID: AtomicU64 = AtomicU64::new(0);

// `Relaxed`: pure tallies. See `kernel::percpu` for the kernel-wide
// ordering discipline.
static TASK_A_LOOPS: AtomicU64 = AtomicU64::new(0);
static TASK_B_LOOPS: AtomicU64 = AtomicU64::new(0);

/// Burn an appreciable amount of CPU so the per-task `cpu_time_ticks`
/// rate is visible against idle's wfi-dominated time. ~15M LCG iters
/// is ~50ms of wallclock on QEMU virt; task_b doubles it.
///
/// `black_box(x)` keeps the loop body from being optimised out — the
/// LCG state has to look observable to the compiler.
fn burn_lcg(iterations: u32) {
    let mut x: u64 = 1;
    for _ in 0..iterations {
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    }
    let _ = core::hint::black_box(x);
}

#[cfg_attr(any(feature = "deflake-spawn-storm", feature = "deflake-ipi-pong", feature = "deflake-shootdown-storm"), allow(dead_code))]
extern "C" fn task_a_entry() -> ! {
    loop {
        {
            span!("task_a.tick");
            // Split the work around a yield to exercise the
            // "span survives a context switch" path. Per-task
            // SpanCursor means task_b's spans opened in between
            // don't get parented to this still-open span.
            burn_lcg(150_000);
            sched::yield_now();
            burn_lcg(150_000);
            TASK_A_LOOPS.fetch_add(1, Ordering::Relaxed);
        }
        sched::yield_now();
    }
}

#[cfg_attr(any(feature = "deflake-spawn-storm", feature = "deflake-ipi-pong", feature = "deflake-shootdown-storm"), allow(dead_code))]
extern "C" fn task_b_entry() -> ! {
    loop {
        {
            span!("task_b.tick");
            burn_lcg(900_000);
            TASK_B_LOOPS.fetch_add(1, Ordering::Relaxed);
        }
        sched::yield_now();
    }
}

/// Recursion guard for the panic handler. Set on entry; if already set, we
/// must already be panicking and shouldn't try to print again (formatting the
/// panic info could itself panic, leading to infinite recursion).
///
/// `Relaxed` on the `swap`: the guard prevents *re-entry on this same hart*
/// (formatting that itself panics). The atomic value is the whole signal;
/// no payload to publish. SMP later: `scaling-corners.md` documents
/// "any hart panics → whole system panics" as the v0.1 contract — when
/// fault isolation lands this will become a per-hart guard.
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
