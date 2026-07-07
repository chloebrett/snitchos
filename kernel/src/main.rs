#![no_std]
#![no_main]

extern crate alloc;

use core::arch::global_asm;
use core::sync::atomic::Ordering;
use fdt::Fdt;
use kernel_core::bootargs::WorkloadKind;

// Modules are grouped by concern into directory modules (`mem/`, `device/`,
// `smp/`, `obs/`, `workloads/`, plus submodules under `sched`/`trap`). Each
// group re-exports its children at the crate root via `pub(crate) use` below, so
// call sites stay `crate::frame`, `crate::trap::user`, etc. — the physical
// nesting doesn't change the logical paths.
mod device;
mod dtb;
mod mem;
mod obs;
mod panic;
mod sbi;
mod sched;
mod smp;
mod syscall;
mod trap;
mod workloads;

pub(crate) use device::{console, uart, virtio_console};
pub(crate) use mem::{frame, heap, heap_smoke, mmu};
pub(crate) use obs::{counter, heartbeat, tracing};
pub(crate) use sched::{demo_tasks, process};
pub(crate) use smp::{ipi, percpu, secondary, sync};
pub(crate) use trap::{ipc, user};
pub(crate) use workloads::{boot_workload, workload};
#[cfg(feature = "itest-workloads")]
pub(crate) use workloads::storms;

// Pull in the boot stub (entry.S). It defines `_start`, sets up the stack
// pointer, zeros .bss, and calls `kmain`. See linker.ld for the memory layout
// it depends on (__stack_top, __bss_start, __bss_end).
global_asm!(include_str!("entry.S"));

/// Write a fixed greeting to the ns16550a UART and halt, without enabling
/// paging. The UART is identity-accessible while the MMU is off, so this needs
/// nothing the trampoline sets up — it's the console-out smoke for an emulator
/// that doesn't model Sv39 yet (snemu milestone 1).
#[cfg(feature = "minimal-boot")]
fn minimal_boot() -> ! {
    const UART_THR: *mut u8 = 0x1000_0000 as *mut u8;
    const UART_LSR: *const u8 = 0x1000_0005 as *const u8;
    const LSR_THRE: u8 = 0x20; // transmit holding register empty

    for &byte in b"Hello from snemu (minimal-boot)\n" {
        // SAFETY: ns16550a MMIO, identity-mapped with the MMU off.
        while unsafe { core::ptr::read_volatile(UART_LSR) } & LSR_THRE == 0 {}
        unsafe { core::ptr::write_volatile(UART_THR, byte) };
    }
    loop {
        // SAFETY: `wfi` is always valid in S-mode.
        unsafe { core::arch::asm!("wfi") };
    }
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
#[unsafe(no_mangle)]
#[cfg_attr(
    feature = "minimal-boot",
    allow(
        unreachable_code,
        unused,
        reason = "minimal_boot diverges before the normal boot path"
    )
)]
pub extern "C" fn kmain(_hart_id: usize, dtb_phys: usize) -> ! {
    // snemu milestone-1 console-out smoke: greet and halt before any paging.
    #[cfg(feature = "minimal-boot")]
    minimal_boot();

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
        .expect("DTB missing /cpus/timebase-frequency — can't run without a clock")
        as u64;

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

    println!("I am alive");

    // Register the metric set early — BEFORE timer init, spawns, or
    // anything else that might compete with us for CPU. Each
    // `register_*` call emits two virtio frames (StringRegister +
    // MetricRegister), so ~35 metrics ≈ 70 sends, each blocking on
    // the device. Doing this after task spawns would interleave the
    // sends with workload-task time slices, starving them. The
    // registered `Metrics` is held until `heartbeat::run` consumes
    // it at the end of kmain.
    let metrics = heartbeat::Metrics::register();
    // Intern the `DeferredCounter` registry's names too, at boot — same
    // rationale as `Metrics::register` (keep the `StringRegister` sends off the
    // task-time path). The heartbeat then drains them via `counter::drain_all`.
    counter::register_all();

    // Arm the periodic timer and enable interrupts. From here on, the
    // CPU wakes us via timer IRQ instead of us spinning on the cycle
    // counter.
    //
    // SAFETY: trap vector was installed at the top of kmain; the
    // handler is ready. The interval is the *fast tick* (heartbeat period ÷
    // `TICKS_PER_HEARTBEAT`): the timer fires often for responsive console-RX
    // drain + preemption, while the heartbeat still runs at the per-second
    // cadence (gated in the handler). One heartbeat period = `timebase_hz` ticks.
    unsafe { trap::init_timer(timebase_hz / trap::TICKS_PER_HEARTBEAT) };
    // Publish the raw timebase for the `ClockFreq` syscall (userspace `Instant`
    // divides a `ClockNow` tick delta by this to get a real `Duration`).
    trap::TIMEBASE_HZ.store(u64::from(timebase_hz), Ordering::Relaxed);

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

    // Install the guard-paged kstack window's shared root subtree (root PTE 257)
    // before any spawn or user address space, so every process sees kernel-stack
    // mappings (a task runs on its kernel stack under its own `satp`). After
    // `heap::init` (needs frames + the live linear map), before the first spawn.
    sched::init_stack_window();

    // Punch a guard page below the boot stack (task 0) — splits the 2 MiB
    // kernel-image leaf and unmaps the guard so a boot-stack overflow faults
    // instead of corrupting `.bss`. Boot hart only, after the frame allocator is
    // up (the split needs a page-table frame).
    mmu::guard_boot_stack();

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
    // Boot workload selection. The default is the standard demo
    // (task_a, task_b, producer, consumer on hart 0). An
    // `itest-workloads` build may override it at runtime from the
    // `workload=` kernel bootarg (QEMU `-append`); production builds
    // never consult bootargs and always run the default — the registry
    // is purely additive. See
    // `docs/runtime-workload-selection-design.md`.
    //
    // Resolved and published *before any task is created*: the spawn
    // path (`Task::new_bare`, `sched::spawn`) reads it back via
    // `boot_workload::selected()` (the spawn storm suppresses per-task
    // counters + ThreadRegister), as does the heartbeat (the OOM
    // workloads change the per-tick smoke). Only `itest-workloads`
    // builds consult bootargs; everything else resolves to `None`.
    #[cfg(feature = "itest-workloads")]
    let bootargs: Option<&str> = dtb.chosen().bootargs();
    #[cfg(not(feature = "itest-workloads"))]
    let bootargs: Option<&str> = None;
    let selected: Option<WorkloadKind> = bootargs.and_then(kernel_core::bootargs::select);
    boot_workload::init(selected);
    // Optional `burst=N` tunes how many batches the producer/consumer
    // run per yield — used to dial up `Mutex` contention for the
    // mutex-vs-spsc measurement. Absent → default (1, low contention).
    if let Some(n) = bootargs.and_then(|a| kernel_core::bootargs::param_usize(a, "burst")) {
        workload::set_burst(n);
    }

    let _ = sched::register_bare_task("main", kernel_core::sched::TaskState::Running);
    let _ = sched::spawn("idle", demo_tasks::idle_entry);
    // v0.5.x exit smoke: one task that bumps a counter then calls
    // `exit_now`. Asserts the asm + state-machine + snapshot-filter
    // wire together without crashing the kernel. Costs one 16 KiB
    // leaked stack at boot.
    let _ = sched::spawn("exit_smoke", sched::exit_smoke_entry);

    // Pre-secondary spawns. The cross-hart workloads (SMP, the task
    // storms) place their hart-1 tasks after `SECONDARY_READY` below;
    // the heartbeat-driven storms spawn nothing.
    match selected {
        Some(WorkloadKind::Smp) => {
            // Cross-hart: producer on hart 0 here, consumer on hart 1
            // after SECONDARY_READY. The `Mutex<VecDeque>` queue carries
            // real inter-hart contention; task_a/task_b are absent to
            // keep hart 0's surface clean for measurement.
            let _ = sched::spawn("workload_producer", workload::producer_entry);
        }
        Some(WorkloadKind::SmpSpsc) => {
            // Same as Smp but over a lock-free `heapless::spsc` ring.
            // Split the ring before either endpoint's task can run.
            workload::init_spsc();
            let _ = sched::spawn("workload_producer", workload::spsc_producer_entry);
        }
        Some(WorkloadKind::SmpSpscBatch) => {
            // Lock-free ring that fences per-batch (shared static, no split).
            let _ = sched::spawn("workload_producer", workload::spsc_batch_producer_entry);
        }
        // The kernel scheduler demo (`workload=demo`, the former default) and the
        // OOM workloads, which keep the standard tasks and only change the
        // heartbeat. The no-bootarg default (`None`) now boots `init` instead —
        // it spawns no hart-0 demo tasks here; init is realised post-secondary via
        // the userspace layout (see below).
        Some(WorkloadKind::Demo) | Some(WorkloadKind::FrameOom) | Some(WorkloadKind::HeapOom) => {
            let _ = sched::spawn("task_a", demo_tasks::task_a_entry);
            let _ = sched::spawn("task_b", demo_tasks::task_b_entry);
            let _ = sched::spawn("workload_producer", workload::producer_entry);
            let _ = sched::spawn("workload_consumer", workload::consumer_entry);
        }
        // v0.9 block/wake smoke: a blocker + waker on hart 0. The blocker
        // calls `block_current`; the waker `wake`s it. Single-hart, kernel
        // tasks — no hart-1 placement (skipped from the probe below).
        Some(WorkloadKind::BlockWake) => {
            let _ = sched::spawn("blocker", sched::block_wake_blocker_entry);
            let _ = sched::spawn("waker", sched::block_wake_waker_entry);
        }
        // Storms spawn post-secondary (task storms) or are entirely
        // heartbeat-driven (spawn/ipi/shootdown).
        Some(WorkloadKind::SpawnStorm)
        | Some(WorkloadKind::IpiPong)
        | Some(WorkloadKind::ShootdownStorm)
        | Some(WorkloadKind::MutexStorm)
        | Some(WorkloadKind::VirtioStorm)
        | Some(WorkloadKind::TlbShootdownVisible)
        | Some(WorkloadKind::PingPong)
        // Userspace: hart 0 just heartbeats; the user program is placed on
        // hart 1 after SECONDARY_READY.
        | Some(WorkloadKind::Userspace)
        | Some(WorkloadKind::UserspaceFault)
        | Some(WorkloadKind::UserspaceBadPtr)
        | Some(WorkloadKind::UserspaceSpanFlood)
        | Some(WorkloadKind::Workers)
        | Some(WorkloadKind::HeapGrow)
        | Some(WorkloadKind::UserHog)
        | Some(WorkloadKind::SyscallHog)
        | Some(WorkloadKind::ConsoleEcho)
        | Some(WorkloadKind::StitchRepl)
        | Some(WorkloadKind::StitchFs)
        | Some(WorkloadKind::SpawnImage)
        | Some(WorkloadKind::ManifestIface)
        | Some(WorkloadKind::ManifestSatisfy)
        | Some(WorkloadKind::Probe)
        | Some(WorkloadKind::StackGuard)
        | Some(WorkloadKind::PanicNow)
        | Some(WorkloadKind::StackOverflowDeep)
        | Some(WorkloadKind::BootStackGuard)
        | Some(WorkloadKind::SpawnDemo)
        | Some(WorkloadKind::SpawnReap)
        | Some(WorkloadKind::WaitAny)
        | Some(WorkloadKind::Init)
        | Some(WorkloadKind::EndpointCreate)
        | Some(WorkloadKind::NotifySmoke)
        | Some(WorkloadKind::Priorities)
        | Some(WorkloadKind::Ipc)
        | Some(WorkloadKind::IpcRpc)
        | Some(WorkloadKind::BadgeMint)
        | Some(WorkloadKind::BadgeHandout)
        | Some(WorkloadKind::Fs)
        | Some(WorkloadKind::ViewerDemo)
        | Some(WorkloadKind::Shell)
        // The no-bootarg default boots `init` (userspace, placed on hart 1 via the
        // layout below) — nothing to spawn on hart 0 here.
        | None => {}
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
    // runqueue + IPI wakeup nudges hart 1 to pick it. Skipped for the
    // storm workloads (which drive hart 1 themselves), the userspace
    // workloads (which place their program on hart 1), and the no-bootarg
    // default (`None` → `init`, also a hart-1 userspace program) — hence
    // `map_or(true, …)`: `None` skips the probe just like a userspace workload.
    if !selected.map_or(true, |w| {
        w.is_storm()
            || matches!(
                w,
                WorkloadKind::Userspace
                    | WorkloadKind::UserspaceFault
                    | WorkloadKind::UserspaceSpanFlood
                    | WorkloadKind::Workers
                    | WorkloadKind::HeapGrow
                    | WorkloadKind::UserHog
                    | WorkloadKind::SyscallHog
                    | WorkloadKind::ConsoleEcho
                    | WorkloadKind::StitchRepl
                    | WorkloadKind::StitchFs
                    | WorkloadKind::SpawnImage
                    | WorkloadKind::ManifestIface
                    | WorkloadKind::ManifestSatisfy
                    | WorkloadKind::SpawnDemo
                    | WorkloadKind::SpawnReap
                    | WorkloadKind::WaitAny
                    | WorkloadKind::Init
                    | WorkloadKind::EndpointCreate
                    | WorkloadKind::UserspaceBadPtr
                    | WorkloadKind::Priorities
                    | WorkloadKind::BlockWake
                    | WorkloadKind::Ipc
                    | WorkloadKind::IpcRpc
                    | WorkloadKind::BadgeMint
                    | WorkloadKind::BadgeHandout
                    | WorkloadKind::Fs
            )
    }) {
        let _ = sched::spawn_on(1, "hart_1_probe", secondary::probe_entry);
    }

    // v0.6 step 11/12: place the consumer on hart 1 for the cross-hart
    // workloads. `spawn_on` enqueues it on hart 1's runqueue and IPIs
    // the hart so its idle `wfi` wakes to pick it up. Producer (hart 0)
    // and consumer (hart 1) then share the queue across the boundary —
    // contending on the `Mutex` (Smp) or lock-free over SPSC (SmpSpsc).
    match selected {
        Some(WorkloadKind::Smp) => {
            let _ = sched::spawn_on(1, "workload_consumer", workload::consumer_entry);
        }
        Some(WorkloadKind::SmpSpsc) => {
            let _ = sched::spawn_on(1, "workload_consumer", workload::spsc_consumer_entry);
        }
        Some(WorkloadKind::SmpSpscBatch) => {
            let _ = sched::spawn_on(1, "workload_consumer", workload::spsc_batch_consumer_entry);
        }
        _ => {}
    }

    // Userspace workloads are table-driven: each declares its program set +
    // endpoint need in `user::user_layout`. Here we just realise it — register
    // the telemetry counter, create the shared endpoint if needed, and spawn
    // each program on hart 1 (hart 0 keeps heartbeating). Adding a userspace
    // workload is a `ProgramSpec` + a `user_layout` row — no spawn arm here.
    // The no-bootarg default boots `init` — resolve `None` to its layout so the
    // default boot realises the userspace root. Named userspace workloads use their
    // own layout; non-userspace selections (demo, SMP, storms) have none.
    if let Some(layout) = user::user_layout(selected.unwrap_or(WorkloadKind::Init)) {
        user::init_metric();
        if layout.needs_endpoint {
            // The shared workload endpoint is the FS server's in the fs workloads;
            // name it so `hold` shows `for=fs` (see capability-names-design.md).
            crate::ipc::DEMO_ENDPOINT.call_once(|| crate::ipc::create(snitchos_abi::pack_name("fs")));
        }
        for p in layout.programs {
            let _ = user::spawn_program(1, p.name, p.program, p.priority);
        }
    }

    // Task-driven storms: spawn their hart-0 + hart-1 bodies now that
    // the secondary is online. Only present in `itest-workloads`
    // builds; the heartbeat-driven storms (spawn/ipi/shootdown) run
    // from the heartbeat tick and spawn nothing here.
    #[cfg(feature = "itest-workloads")]
    match selected {
        // Two contenders hammer a shared `Mutex`; each does N
        // lock/unlock then `exit_now`. Heartbeat re-emits progress.
        Some(WorkloadKind::MutexStorm) => {
            let _ = sched::spawn("mutex_storm_h0", storms::mutex_storm::body_hart0);
            let _ = sched::spawn_on(1, "mutex_storm_h1", storms::mutex_storm::body_hart1);
        }
        // Pre-register the emission metric (so its StringRegister isn't
        // emitted from inside the storm loop, muddying per-iteration
        // timing), then spawn both bodies: hart 0 emits, hart 1 spins.
        Some(WorkloadKind::VirtioStorm) => {
            storms::virtio_storm::init();
            let _ = sched::spawn("virtio_storm_h0", storms::virtio_storm::body_hart0);
            let _ = sched::spawn_on(1, "virtio_storm_h1", storms::virtio_storm::body_hart1);
        }
        // TLB-shootdown correctness: hart 0's round loop is
        // heartbeat-driven (`emit_storm_metrics`); here we spawn the
        // hart-1 reader that holds the stale translation under test.
        Some(WorkloadKind::TlbShootdownVisible) => {
            let _ = sched::spawn_on(1, "tlb_reader", storms::tlb_shootdown::reader_body);
        }
        // Cross-hart ping-pong: pong on hart 1; ping is heartbeat-driven
        // on hart 0 (`emit_storm_metrics`). They alternate a shared turn
        // flag in lockstep.
        Some(WorkloadKind::PingPong) => {
            let _ = sched::spawn_on(1, "pong", storms::ping_pong::pong_body);
        }
        // Tier-B guard-page smoke: a kernel task stores into its own guard page on
        // hart 0; the trap handler recognizes the guard region, snitches + panics.
        Some(WorkloadKind::StackGuard) => {
            let _ = sched::spawn("stack_guard_smoke", storms::stack_guard::smoke_body);
        }
        // Minimal crash: a task that panics immediately (no guard page). Isolates
        // the stack-guard family's snemu-vs-QEMU `kernel.heartbeat` divergence.
        Some(WorkloadKind::PanicNow) => {
            let _ = sched::spawn("panic_now", storms::panic_now::body);
        }
        // Tier-B deep-overflow smoke: a kernel task recurses into its guard page;
        // the fault handler (on the per-hart exception stack) reports cleanly.
        Some(WorkloadKind::StackOverflowDeep) => {
            let _ = sched::spawn("stack_overflow_deep", storms::stack_overflow_deep::smoke_body);
        }
        // Boot-stack guard smoke: a kernel task stores into the boot stack's guard
        // page; the trap handler recognizes the boot guard region, snitches + panics.
        Some(WorkloadKind::BootStackGuard) => {
            let _ = sched::spawn("boot_stack_guard", storms::boot_stack_guard::smoke_body);
        }
        _ => {}
    }

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

    println!("entering heartbeat");
    heartbeat::run(metrics)
}
