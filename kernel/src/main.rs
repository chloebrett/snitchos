#![no_std]
#![no_main]

extern crate alloc;

use core::arch::global_asm;
use core::sync::atomic::Ordering;
use fdt::Fdt;
use kernel_boot::bootargs::WorkloadKind;

// Modules are grouped by concern into directory modules (`mem/`, `device/`,
// `smp/`, `obs/`, `workloads/`, plus submodules under `sched`/`trap`). Each
// group re-exports its children at the crate root via `pub(crate) use` below, so
// call sites stay `crate::frame`, `crate::trap::user`, etc. — the physical
// nesting doesn't change the logical paths.
mod banner;
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

pub(crate) use device::{console, fwcfg, ramfb, uart, virtio_console};
pub(crate) use mem::{frame, heap, heap_smoke, mmu};
pub(crate) use obs::{counter, heartbeat, tracing};
pub(crate) use sched::{demo_tasks, process};
pub(crate) use smp::{ipi, percpu, secondary, sync};
pub(crate) use trap::{ipc, user};
#[cfg(feature = "itest-workloads")]
pub(crate) use workloads::storms;
pub(crate) use workloads::{boot_workload, workload};

/// **Temporary** board bring-up instrumentation: print a boot-phase marker on
/// the human UART. `vf2`-only, so QEMU's console stays quiet and no host test
/// sees it.
///
/// This exists because the board has no telemetry transport yet (M2/B3), so a
/// hang between two boot steps is completely silent — the only evidence is
/// *which marker was the last one printed*. An earlier sweep deleted these as
/// debt and immediately cost us that evidence on the next board hang; they stay
/// until real frames replace them, at which point deleting the macro deletes
/// every call site's output at once.
macro_rules! ph {
    ($($arg:tt)*) => {
        #[cfg(feature = "vf2")]
        crate::println!("ph: {}", format_args!($($arg)*));
    };
}

// Pull in the boot stub (entry.S). It defines `_start`, sets up the stack
// pointer, zeros .bss, and calls `kmain`. See linker.ld for the memory layout
// it depends on (__stack_top, __bss_start, __bss_end).
global_asm!(include_str!("entry.S"));

/// Kernel entry point, called from `_start` (see entry.S).
///
/// Inputs come from `OpenSBI`'s S-mode handoff contract:
/// - `hart_id`: the **mhartid** of the hart we booted on. Not a logical id and
///   not necessarily 0 — `OpenSBI` picks the boot hart, and under QEMU `-smp 2`
///   it can hand us 1. Everything downstream that says "the other hart" derives
///   from it (see the `1 - hart_id` arithmetic below).
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
pub extern "C" fn kmain(hart_id: usize, dtb_phys: usize) -> ! {
    // Nothing here may outlive this function: `kmain`'s frame is allocated with a
    // *physical* `sp` (see the hand-off comment at the end). The DTB is parsed in
    // `kmain_higher_half` instead — it isn't needed here, because MMIO discovery is
    // hardcoded. (If `collect_mmio_regions` is ever revived — B5 — it would need a
    // pre-MMU parse right here, and its results must be *values*, not pointers.)
    //
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

    // Hand off to a *fresh* stack frame. `kmain`'s own frame was allocated with a
    // **physical** `sp` (entry.S enters pre-MMU), and the shift above only
    // re-aliases those same bytes — so any address of a `kmain` local that the
    // compiler materialised or spilled *before* this point stays physical forever.
    // Such a pointer works only while the identity map is live and faults the
    // instant `unmap_identity` runs. Calling a separate function means every local
    // from here on gets its address computed against the higher-half `sp`.
    //
    // Third instance of the "address materialised on the wrong side of the
    // trampoline" family, after the `tp` truncation and the `entry_pa`
    // loop-invariant miscompile — see the ⚠ callout in plans/visionfive2-port.md.
    // The rule: no cached address, and no stack frame, may span the trampoline.
    kmain_higher_half(hart_id, dtb_phys)
}

/// The boot sequence proper, running entirely at higher-half PC **and** `sp`.
///
/// Split out of [`kmain`] so this frame is allocated *after* the trampoline — see
/// the hand-off comment there for why that matters. `#[inline(never)]` is
/// load-bearing: inlining this back into `kmain` would recreate the straddling
/// frame and the bug with it.
#[inline(never)]
fn kmain_higher_half(hart_id: usize, dtb_phys: usize) -> ! {
    // Parsed here rather than pre-trampoline so the `Fdt` — and every address
    // derived from it — belongs to this frame. The DTB bytes stay reachable
    // through the identity map until `unmap_identity`; the borrow is dropped
    // before that (`let _ = dtb` below).
    let dtb = unsafe { Fdt::from_ptr(dtb_phys as *const u8) }.unwrap();

    // v0.6 step 4: install the per-hart pointer. `PER_HART_DATA` is a
    // higher-half static, so this must run post-trampoline.
    //
    // We pass *logical* hartid 0 here, regardless of what mhartid
    // OpenSBI handed us as `hart_id`. The boot hart is by definition
    // logical hart 0 (kmain runs there once, owns all the boot
    // bookkeeping); the secondary always comes up as logical hart 1
    // via `secondary_main(_, 1)`. The platform `mhartid` is captured
    // separately into `BOOT_MHARTID` for telemetry; the dense logical
    // id is what every other piece of the kernel reasons about.
    //
    // SAFETY: trampoline executed (higher-half VAs resolve); no
    // per-hart-aware code has run yet.
    unsafe { percpu::init(0) };

    // Enumerate the harts the DTB advertises and assign dense logical ids: the
    // boot hart (mhartid `hart_id`) becomes logical 0, the other *usable* harts
    // follow in ascending mhartid order. `ipi::send(logical_id)` then translates
    // through `LOGICAL_TO_MHARTID` to the platform mhartid `sbi_send_ipi` expects.
    //
    // This replaces the old `{ 0 → hart_id, 1 → 1-hart_id }` two-hart arithmetic,
    // which underflowed on a boot hartid > 1 — the VisionFive 2 boots on an
    // arbitrary U74 (harts 1–4). The JH7110's S7 monitor (`status="disabled"`) is
    // filtered out by `is_usable`. `num_harts` (carried to bring-up below) is how
    // many we run; on QEMU `-smp 2` it is 2. Alloc-free fixed buffers: the cpu
    // list holds the DTB's cpus (S7 + up to 4 U74s), the map holds `MAX_HARTS`.
    let mut hart_list = [kernel_boot::harts::HartInfo::default(); 8];
    let n_listed = dtb::enumerate_harts(&dtb, &mut hart_list);
    let mut mhartid_map = [0u64; percpu::MAX_HARTS];
    let num_harts = kernel_boot::harts::assign_logical(
        &hart_list[..n_listed],
        hart_id as u64,
        &mut mhartid_map,
    );
    for (logical, &mhartid) in mhartid_map.iter().enumerate().take(num_harts) {
        percpu::LOGICAL_TO_MHARTID[logical].store(mhartid, Ordering::Relaxed);
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

    let timebase_hz = u64::from(
        dtb::timebase_hz(&dtb)
            .expect("DTB missing /cpus/timebase-frequency — can't run without a clock"),
    );

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

        let (uart_base, uart_reg_shift, uart_io_width) = dtb::uart_config(&dtb);
        {
            span!("console_init");
            // SAFETY: the config came from the DTB's console UART node, and
            // `mmu::enable` has run so the higher-half MMIO mapping is live.
            unsafe { console::init(uart_base, uart_reg_shift, uart_io_width) };
        }

        dtb::print_info(&dtb, uart_base);

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

    ph!("pre-banner");
    banner::print();
    ph!("post-banner");

    // Register the metric set early — BEFORE timer init, spawns, or
    // anything else that might compete with us for CPU. Each
    // `register_*` call emits two virtio frames (StringRegister +
    // MetricRegister), so ~35 metrics ≈ 70 sends, each blocking on
    // the device. Doing this after task spawns would interleave the
    // sends with workload-task time slices, starving them. The
    // registered `Metrics` is held until `heartbeat::run` consumes
    // it at the end of kmain.
    let metrics = heartbeat::Metrics::register();
    ph!("metrics registered");
    // Intern the `DeferredCounter` registry's names too, at boot — same
    // rationale as `Metrics::register` (keep the `StringRegister` sends off the
    // task-time path). The heartbeat then drains them via `counter::drain_all`.
    counter::register_all();
    ph!("counters registered");

    // Arm the periodic timer and enable interrupts. From here on, the
    // CPU wakes us via timer IRQ instead of us spinning on the cycle
    // counter.
    //
    // SAFETY: trap vector was installed at the top of kmain; the
    // handler is ready. The interval is the *fast tick* (heartbeat period ÷
    // `TICKS_PER_HEARTBEAT`): the timer fires often for responsive console-RX
    // drain + preemption, while the heartbeat still runs at the per-second
    // cadence (gated in the handler). One heartbeat period = `timebase_hz` ticks.
    ph!("pre-timer");
    unsafe { trap::init_timer(timebase_hz / trap::TICKS_PER_HEARTBEAT) };
    ph!("post-timer (interrupts on)");
    // Publish the raw timebase for the `ClockFreq` syscall (userspace `Instant`
    // divides a `ClockNow` tick delta by this to get a real `Duration`).
    trap::TIMEBASE_HZ.store(timebase_hz, Ordering::Relaxed);

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
    ph!("post-ipi");

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
    ph!("post-frame-alloc");

    // Kernel heap. Pulls a contiguous run of frames from the frame
    // allocator and hands their linear-map VA to
    // `linked_list_allocator`. After this, anything in `alloc::`
    // (`Box`, `Vec`, formatted strings that exceed the stack buffer)
    // works inside the kernel.
    //
    // SAFETY: called exactly once, after frame allocator init, with
    // the linear map live (installed by `mmu::enable`).
    unsafe { heap::init() };
    ph!("heap init");

    // Install the guard-paged kstack window's shared root subtree (root PTE 257)
    // before any spawn or user address space, so every process sees kernel-stack
    // mappings (a task runs on its kernel stack under its own `satp`). After
    // `heap::init` (needs frames + the live linear map), before the first spawn.
    sched::init_stack_window();
    ph!("stack window");

    // Punch a guard page below the boot stack (task 0) — splits the 2 MiB
    // kernel-image leaf and unmaps the guard so a boot-stack overflow faults
    // instead of corrupting `.bss`. Boot hart only, after the frame allocator is
    // up (the split needs a page-table frame).
    mmu::guard_boot_stack();
    ph!("guard boot stack");

    // Framebuffer bring-up (Milestone 0). Best-effort: a machine booted
    // without `-device ramfb` has no `etc/ramfb` fw_cfg file, so `init`
    // snitches a refusal (`INIT_REFUSED`) and boot continues with no
    // display — never fatal.
    //
    // SAFETY: called exactly once, after heap::init (needs frames + the
    // live linear map) and after mmu::enable, before any other user of
    // root PTE slot 258.
    // ramfb rides QEMU's `fw_cfg` device, which the VisionFive 2 doesn't have —
    // and driving an absent device is worse than having no device, because
    // `write_file` polls a DMA control bit that nothing will ever clear. So ask
    // the DTB whether the device exists before touching its registers at all.
    // A real display on the board needs a JH7110 DC/HDMI driver (out of scope);
    // see "dropped — framebuffer" in plans/visionfive2-port.md.
    ph!("pre-ramfb (fw_cfg in dtb: {})", dtb::has_fw_cfg(&dtb));
    if dtb::has_fw_cfg(&dtb) {
        let _ = unsafe { ramfb::init() };
    }
    ph!("post-ramfb");

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
    ph!("sched smoke");

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
    // Bootargs are read in *every* build. The registry is additive, but it isn't
    // all scaffolding: `Init`, `Userspace` and `StitchRepl` are real features, and
    // gating the reader on `itest-workloads` meant shipping the Stitch REPL to the
    // board required a test-only feature flag. What stays behind the feature is the
    // *storm bodies* (`kernel::storms`) and their spawn arms — so a production build
    // that's handed `workload=mutex-storm` parses it, finds no dispatch arm, and
    // boots the default rather than growing test code.
    let bootargs: Option<&str> = dtb.chosen().bootargs();
    let selected: Option<WorkloadKind> = bootargs.and_then(kernel_boot::bootargs::select);
    boot_workload::init(selected);
    // Optional `burst=N` tunes how many batches the producer/consumer
    // run per yield — used to dial up `Mutex` contention for the
    // mutex-vs-spsc measurement. Absent → default (1, low contention).
    if let Some(n) = bootargs.and_then(|a| kernel_boot::bootargs::param_usize(a, "burst")) {
        workload::set_burst(n);
    }

    let _ = sched::register_bare_task("main", kernel_proc::sched::TaskState::Running);
    let _ = sched::spawn("idle", demo_tasks::idle_entry);
    // v0.5.x exit smoke: one task that bumps a counter then calls
    // `exit_now`. Asserts the asm + state-machine + snapshot-filter
    // wire together without crashing the kernel. Costs one 16 KiB
    // leaked stack at boot.
    let _ = sched::spawn("exit_smoke", sched::exit_smoke_entry);
    ph!("boot tasks spawned");

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
        // The kernel scheduler demo (`workload=demo`, the former default). The
        // no-bootarg default (`None`) now boots `init` instead — it spawns no
        // hart-0 demo tasks here; init is realised post-secondary via the
        // userspace layout (see below).
        Some(WorkloadKind::Demo) => {
            let _ = sched::spawn("task_a", demo_tasks::task_a_entry);
            let _ = sched::spawn("task_b", demo_tasks::task_b_entry);
            let _ = sched::spawn("workload_producer", workload::producer_entry);
            let _ = sched::spawn("workload_consumer", workload::consumer_entry);
        }
        // Just the cooperative producer/consumer pair — the
        // `workload-cooperative-baseline` oracle without demo's task_a/task_b, so
        // the pair gets every scheduler turn and reaches its sample threshold in
        // far fewer instructions (same assertion, cheaper). See `bootargs`.
        Some(WorkloadKind::Cooperative) => {
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
        // heartbeat-driven (spawn/ipi/shootdown). `live-tasks` fills its table in
        // the post-secondary match below too.
        Some(
            WorkloadKind::SpawnStorm
            | WorkloadKind::LiveTasks
            | WorkloadKind::IpiPong
            | WorkloadKind::ShootdownStorm
            | WorkloadKind::MutexStorm
            | WorkloadKind::VirtioStorm
            | WorkloadKind::TlbShootdown
            | WorkloadKind::PingPong
            | WorkloadKind::Userspace
            | WorkloadKind::UserspaceFault
            | WorkloadKind::UserspaceBadPtr
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
            | WorkloadKind::Probe
            | WorkloadKind::StackGuard
            | WorkloadKind::PanicNow
            | WorkloadKind::StackOverflowDeep
            | WorkloadKind::BootStackGuard
            | WorkloadKind::SpawnDemo
            | WorkloadKind::SpawnReap
            | WorkloadKind::WaitAny
            | WorkloadKind::Init
            | WorkloadKind::Supervised
            | WorkloadKind::SupervisedIpc
            | WorkloadKind::SupervisedShutdown
            | WorkloadKind::KillNoCap
            | WorkloadKind::UserOnHart0
            | WorkloadKind::XhartKill
            | WorkloadKind::HungDetect
            | WorkloadKind::EndpointCreate
            | WorkloadKind::NotifySmoke
            | WorkloadKind::Priorities
            | WorkloadKind::Ipc
            | WorkloadKind::IpcRpc
            | WorkloadKind::BadgeMint
            | WorkloadKind::BadgeHandout
            | WorkloadKind::Fs
            | WorkloadKind::ViewDemo
            | WorkloadKind::Shell
            | WorkloadKind::FrameOom
            | WorkloadKind::HeapOom
            // smp4's workers are placed on the secondaries post-bring-up (below).
            | WorkloadKind::Smp4,
        )
        | None => {}
    }

    // DTB physical region lives in the identity gigapage we're about
    // to tear down. Drop the borrow first to make "no DTB access from
    // here on" load-bearing instead of incidental. `Fdt` has no `Drop`
    // impl, so this is just a binding-scope close.
    let _ = dtb;

    // Declare the boot hart (logical 0, role Boot), then bring up the secondary.
    // The logical→mhartid map was computed from the DTB above, so the secondary's
    // mhartid is `LOGICAL_TO_MHARTID[1]` — no `1 - hart_id` arithmetic (which
    // underflowed on a boot hartid > 1).
    let boot_mhartid = hart_id as u64;
    heartbeat::BOOT_MHARTID.store(boot_mhartid, Ordering::Relaxed);
    tracing::emit_hart_register(0, boot_mhartid, protocol::HartRole::Boot);
    // Bring up each secondary hart (logical `1..num_harts`) in turn. Sequential:
    // each hart latches its stack from `SECONDARY_STACK_TOP` early in `secondary.S`
    // before we rewrite that slot for the next, and we wait on its `SECONDARY_READY`
    // bit — fully up: init + trap vector + timer — before starting the next, which
    // is also the barrier `unmap_identity` needs. A DTB-declared single-hart machine
    // (`num_harts == 1`, e.g. the VisionFive 2 on one U74) runs this loop zero
    // times, so it never `hart_start`s a phantom hart. (Full single-hart boot also
    // needs the `spawn_on(logical, …)` task placement below to target hart 0 —
    // board-M1 follow-up; on QEMU `-smp 2` `num_harts` is 2.)
    for logical in 1..num_harts {
        let secondary_mhartid = percpu::LOGICAL_TO_MHARTID[logical].load(Ordering::Relaxed);
        // Physical entry of `_secondary_start`, recomputed each iteration.
        // Materialize its (higher-half) VA with a side-effecting `lla`, not
        // `_secondary_start as usize`: the plain cast is a pure value the release
        // optimizer mis-hoisted as a loop invariant — it read back **0** on the 2nd
        // iteration, so `hart_start` for logical hart 2 got `start_addr = 0` and
        // that hart faulted at PC 0. Same address-materialization hazard as the `tp`
        // fix — see `plans/v0.4-memory-findings.md` and `kernel/src/smp/percpu.rs`.
        let entry_pa = {
            unsafe extern "C" {
                fn _secondary_start();
            }
            let entry_va: usize;
            // SAFETY: `lla` loads a link-time symbol address into a register; no
            // memory access, no clobbers beyond the named output.
            unsafe {
                core::arch::asm!(
                    "lla {r}, {s}",
                    r = out(reg) entry_va,
                    s = sym _secondary_start,
                    options(nostack, preserves_flags, nomem),
                );
            }
            mmu::va_to_pa(entry_va) as u64
        };
        unsafe { secondary::prepare_for_secondary(logical) };
        #[cfg(feature = "vf2")]
        println!("smp: starting hart {logical} (mhartid {secondary_mhartid})");
        let err = sbi::hart_start(secondary_mhartid, entry_pa, logical as u64);
        assert!(err == 0, "sbi_hart_start({secondary_mhartid}) failed: error={err}");
        // Acquire: pair with the Release on this hart's `SECONDARY_READY` bit.
        let bit = 1u64 << logical;
        while secondary::SECONDARY_READY.load(Ordering::Acquire) & bit == 0 {
            core::hint::spin_loop();
        }
        #[cfg(feature = "vf2")]
        println!("smp: hart {logical} up");
    }
    // v0.6 step 10: cross-hart spawn smoke. Probe lands on hart 1's
    // runqueue + IPI wakeup nudges hart 1 to pick it. Skipped for the
    // storm workloads (which drive hart 1 themselves), the userspace
    // workloads (which place their program on hart 1), and the no-bootarg
    // default (`None` → `init`, also a hart-1 userspace program) — hence
    // `is_none_or`: `None` skips the probe just like a userspace workload.
    if !selected.is_none_or(|w| {
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
                    | WorkloadKind::Supervised
                    | WorkloadKind::SupervisedIpc
                    | WorkloadKind::UserOnHart0
                    | WorkloadKind::XhartKill
                    | WorkloadKind::HungDetect
                    | WorkloadKind::EndpointCreate
                    | WorkloadKind::UserspaceBadPtr
                    | WorkloadKind::Priorities
                    | WorkloadKind::BlockWake
                    | WorkloadKind::Ipc
                    | WorkloadKind::IpcRpc
                    | WorkloadKind::BadgeMint
                    | WorkloadKind::BadgeHandout
                    | WorkloadKind::Fs
                    // smp4 places its own workers on every secondary (below);
                    // it doesn't want the single-hart demo probe on hart 1.
                    | WorkloadKind::Smp4
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
        // The four-hart demo: a worker on every secondary hart. Hart 0 keeps
        // heartbeating; each worker emits `smp4.tick` spans tagged with its hart id.
        Some(WorkloadKind::Smp4) => {
            for logical in 1..num_harts {
                let _ = sched::spawn_on(logical, "smp4_worker", secondary::smp4_worker_entry);
            }
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
    ph!("pre-userspace");
    if let Some(layout) = user::user_layout(selected.unwrap_or(WorkloadKind::Init)) {
        user::init_metric();
        if layout.needs_endpoint {
            // The shared workload endpoint is the FS server's in the fs workloads;
            // name it so `hold` shows `for=fs` (see capability-names-design.md).
            crate::ipc::DEMO_ENDPOINT
                .call_once(|| crate::ipc::create(snitchos_abi::pack_name("fs")));
        }
        // Userspace normally runs on hart 1 (hart 0 heartbeats). The multi-hart
        // de-risk places its program on hart 0 instead, to prove U-mode works there.
        #[allow(
            clippy::bool_to_int_with_if,
            reason = "the if reads as 'hart 0 for UserOnHart0, else hart 1'; \
                      usize::from(selected != …) inverts the sense and obscures it"
        )]
        let user_hart = if selected == Some(WorkloadKind::UserOnHart0) {
            0
        } else {
            1
        };
        for p in layout.programs {
            let _ = user::spawn_program(user_hart, p.name, p.program, p.priority);
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
        Some(WorkloadKind::TlbShootdown) => {
            let _ = sched::spawn_on(1, "tlb_reader", storms::tlb_shootdown::reader_body);
        }
        // Many long-lived tasks: fill the scheduler table with N tasks that
        // loop-yield forever, so every switch resolves ids against a large *live*
        // table — the O(1) `TaskDirectory` stress (`sched-task-lookup-is-o1`).
        Some(WorkloadKind::LiveTasks) => {
            for _ in 0..storms::live_tasks::N {
                let _ = sched::spawn("live", storms::live_tasks::body);
            }
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
            let _ = sched::spawn(
                "stack_overflow_deep",
                storms::stack_overflow_deep::smoke_body,
            );
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
    // Safe on every target again: the post-trampoline boot sequence runs in
    // `kmain_higher_half`'s frame, so it holds no physical self-pointers to be
    // invalidated here. (This briefly had to be skipped on the board — a store to a
    // physical boot-stack address faulted right after this call — which is what
    // uncovered the straddling-frame bug. See the hand-off comment in `kmain`.)
    ph!("pre-unmap-identity");
    unsafe { mmu::unmap_identity() };
    ph!("post-unmap-identity");

    println!("entering heartbeat");
    heartbeat::run(metrics)
}
