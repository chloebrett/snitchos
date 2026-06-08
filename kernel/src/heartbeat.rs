//! Kernel heartbeat. The boot path's `kmain` calls [`run`] once
//! everything else is up; from there main thread becomes a tick-driven
//! metrics emitter. Each tick opens a `kernel.heartbeat` span, runs
//! the per-tick smoke patterns (frame + heap exercise, see feature
//! flags), and emits every metric registered in [`Metrics`].
//!
//! The metric set is built with the [`define_metrics!`] macro: one
//! list, one line per metric, with optional `#[cfg]` per line. The
//! macro generates both the struct declaration and the
//! `register()` constructor, so adding or removing a metric is a
//! one-line edit. Inside [`run`], the [`emit!`] macro wraps the
//! `tracing::emit_metric(..., expr as i64)` call so each emission
//! is one line.

use core::sync::atomic::{AtomicU64, Ordering};

use protocol::StringId;

use crate::{boot_workload, frame, heap, heap_smoke, ipi, mmu, percpu, sched, secondary, span, tracing, trap, workload};

/// Declarative metric set. Each line names a kind (`counter`,
/// `gauge`, `histogram`), a struct field, and the wire path. The
/// expansion produces:
///
/// ```ignore
/// pub struct Metrics { field: StringId, ... }
/// impl Metrics {
///     pub fn register() -> Self { Self { field: tracing::register_*(path), ... } }
/// }
/// ```
///
/// `#[cfg]` (or any other attribute) applied to a line is forwarded
/// to both the struct field and the initialiser, so feature gating
/// each metric is a single annotation.
macro_rules! define_metrics {
    (
        $(
            $(#[$attr:meta])*
            $kind:ident $name:ident = $path:literal;
        )*
    ) => {
        pub struct Metrics {
            $(
                $(#[$attr])*
                $name: StringId,
            )*
        }

        impl Metrics {
            pub fn register() -> Self {
                Self {
                    $(
                        $(#[$attr])*
                        $name: define_metrics!(@reg $kind $path),
                    )*
                }
            }
        }
    };
    (@reg counter   $path:literal) => { tracing::register_counter($path) };
    (@reg gauge     $path:literal) => { tracing::register_gauge($path) };
    (@reg histogram $path:literal) => { tracing::register_histogram($path) };
}

/// `emit!(m, field = expr)` ⇒ `tracing::emit_metric(m.field, expr as i64)`.
/// Centralises the `as i64` cast and the `m.field` access so each
/// per-tick emit site is one line.
macro_rules! emit {
    ($m:expr, $field:ident = $value:expr) => {
        tracing::emit_metric($m.$field, $value as i64)
    };
}

/// mhartid OpenSBI handed kmain as `_hart_id`. Captured at the top of
/// the bring-up block; drained by the heartbeat as
/// `snitchos.smp.boot_hart_id`. Useful because OpenSBI's choice of
/// boot hart is not always 0 under `-smp 2`. `Relaxed`: single writer
/// (boot path), read by heartbeat on the same hart.
pub static BOOT_MHARTID: AtomicU64 = AtomicU64::new(0);

// All `StringId`s for the metrics the heartbeat emits. Constructed
// once via Metrics::register right before run() takes over; the
// register calls are idempotent so re-running them would be safe but
// wasteful (each does a hash-table insert + frame emit).
define_metrics! {
    counter   heartbeat_count           = "snitchos.heartbeat.count";
    gauge     intern_used               = "snitchos.intern.strings_used";
    gauge     time_ticks                = "snitchos.time.ticks";
    histogram irq_duration              = "snitchos.irq.timer.duration_ticks";
    // frame allocator
    counter   frames_allocated          = "snitchos.frames.allocated_total";
    counter   frames_freed              = "snitchos.frames.freed_total";
    counter   frames_alloc_failed       = "snitchos.frames.alloc_failed_total";
    gauge     frames_in_use             = "snitchos.frames.in_use";
    gauge     frames_free               = "snitchos.frames.free";
    // kernel heap
    counter   heap_alloc_total          = "snitchos.heap.alloc_total";
    counter   heap_dealloc_total        = "snitchos.heap.dealloc_total";
    counter   heap_alloc_failed         = "snitchos.heap.alloc_failed_total";
    gauge     heap_bytes_capacity       = "snitchos.heap.bytes_capacity";
    gauge     heap_bytes_used           = "snitchos.heap.bytes_used";
    gauge     heap_bytes_free           = "snitchos.heap.bytes_free";
    counter   heap_grow_total           = "snitchos.heap.grow_total";
    counter   heap_grow_failed          = "snitchos.heap.grow_failed_total";
    gauge     heap_free_blocks          = "snitchos.heap.free_blocks";
    gauge     heap_largest_free_block   = "snitchos.heap.largest_free_block_bytes";
    // scheduler
    counter   sched_smoke_marker_hits   = "snitchos.sched.smoke_marker_hits";
    counter   sched_exit_smoke_hits     = "snitchos.sched.exit_smoke_hits";
    counter   sched_context_switches    = "snitchos.sched.context_switches_total";
    gauge     sched_runqueue_depth      = "snitchos.sched.runqueue_depth";
    gauge     sched_tasks_total         = "snitchos.sched.tasks_total";
    histogram sched_yield_overhead      = "snitchos.sched.yield_overhead_ticks";
    // demo tasks
    counter   task_a_loops              = "snitchos.task_a.loops";
    counter   task_b_loops              = "snitchos.task_b.loops";
    // workload
    counter   workload_produced         = "snitchos.workload.samples_produced_total";
    counter   workload_consumed         = "snitchos.workload.samples_consumed_total";
    gauge     workload_histogram_sum    = "snitchos.workload.histogram_sum";
    counter   workload_lock_wait        = "snitchos.workload.lock_wait_ticks_total";
    gauge     workload_queue_depth      = "snitchos.workload.queue_depth";
    // SMP / IPI
    counter   ipi_received              = "snitchos.ipi.received_total";
    gauge     smp_harts_total           = "snitchos.smp.harts_total";
    gauge     smp_boot_hart_id          = "snitchos.smp.boot_hart_id";
    counter   smp_secondary_wfi         = "snitchos.smp.secondary_wfi_total";
    counter   mmu_shootdowns_received   = "snitchos.mmu.shootdowns_received_total";
    counter   mmu_shootdowns_sent       = "snitchos.mmu.shootdowns_sent_total";
    counter   smp_probe_ticks           = "snitchos.smp.hart_1_probe_ticks_total";
    // SMOKE TEST metrics — remove with heap_smoke module
    gauge     smoke_entries             = "snitchos.heap_smoke.entries";
    gauge     smoke_primes              = "snitchos.heap_smoke.primes";
    gauge     smoke_candidate           = "snitchos.heap_smoke.candidate";
    // deflake scenario metrics
    #[cfg(feature = "deflake-spawn-storm")]
    counter   spawn_storm_acks          = "snitchos.deflake.spawn_storm_acks";
    #[cfg(feature = "deflake-ipi-pong")]
    counter   ipi_pong_sends            = "snitchos.deflake.ipi_pong_sends";
    #[cfg(feature = "deflake-shootdown-storm")]
    counter   shootdown_storm_sends     = "snitchos.deflake.shootdown_storm_sends";
    #[cfg(feature = "deflake-mutex-storm")]
    counter   mutex_storm_acquires_hart0 = "snitchos.deflake.mutex_storm_acquires_hart0";
    #[cfg(feature = "deflake-mutex-storm")]
    counter   mutex_storm_acquires_hart1 = "snitchos.deflake.mutex_storm_acquires_hart1";
    #[cfg(feature = "deflake-virtio-storm")]
    counter   virtio_storm_hart0_emits  = "snitchos.deflake.virtio_storm_hart0_emits";
    #[cfg(feature = "deflake-virtio-storm")]
    counter   virtio_storm_hart1_iterations = "snitchos.deflake.virtio_storm_hart1_iterations";
}

/// Heartbeat main loop. Never returns. Waits for the timer-IRQ
/// handler to flip `TICK_PENDING`; on each tick opens a span, runs
/// the smoke patterns, emits every metric, then yields back to the
/// scheduler.
pub fn run(metrics: Metrics) -> ! {
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
            frame_smoke();
            heap_smoke_pattern(count);
            emit_core(&metrics, count);
            emit_frame_metrics(&metrics);
            emit_heap_metrics(&metrics);
            emit_sched_metrics(&metrics);
            emit_workload_metrics(&metrics);
            emit_smp_metrics(&metrics);
            emit_heap_smoke_metrics(&metrics, count);
            emit_deflake_metrics(&metrics, count);
        }
        sched::yield_now();
    }
}

/// Smoke pattern that exercises the frame allocator each heartbeat.
/// Default: alloc+free, keeps `in_use` bounded. Under the
/// `workload=frame-oom` selection: leak 8192 frames per tick (32 MiB)
/// so the allocator's ~32K-frame free pool exhausts in ~4 heartbeats.
/// Drives the `frame-allocator-oom` integration scenario.
fn frame_smoke() {
    use kernel_core::bootargs::WorkloadKind;
    if boot_workload::selected() == Some(WorkloadKind::FrameOom) {
        for _ in 0..8192 {
            let _ = frame::alloc_zeroed();
        }
    } else if let Some(frame) = frame::alloc_zeroed() {
        frame::free(frame);
    }
}

/// Heap smoke. Default: alloc + write + drop a 256 B Vec — proves the
/// heap is live, keeps `bytes_used` near 0 across heartbeats. Under the
/// `workload=heap-oom` selection: per-heartbeat leak loop using the raw
/// `GlobalAlloc` API (returns null on failure rather than panicking
/// through `alloc_error_handler`). After the heap exhausts, every
/// subsequent iteration's first allocation returns null immediately and
/// bumps `alloc_failed_total` — so the counter climbs once per
/// heartbeat post-OOM, and the kernel keeps heartbeating.
fn heap_smoke_pattern(count: i64) {
    use kernel_core::bootargs::WorkloadKind;
    if boot_workload::selected() != Some(WorkloadKind::HeapOom) {
        let mut v: alloc::vec::Vec<u8> = alloc::vec::Vec::with_capacity(256);
        v.push(count as u8);
        return;
    }
    {
        let _ = count;
        // Leak 4096 × 4 KiB blocks per heartbeat (16 MiB/tick). P2's
        // watermark grow adds 1 MiB/tick, so net pressure is
        // +15 MiB/tick — the ~120 MiB usable RAM (post kernel + bitmap
        // + tables) exhausts in ~8 heartbeats. `try_reserve_exact`
        // returns `Err` rather than panicking; the underlying
        // null-return from `GlobalAlloc::alloc` still bumps
        // `ALLOC_FAIL_COUNT` so the OOM signal makes it to telemetry.
        for _ in 0..4096 {
            let mut v: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
            if v.try_reserve_exact(4096).is_err() {
                break;
            }
            core::mem::forget(v);
        }
    }
}

fn emit_core(m: &Metrics, count: i64) {
    emit!(m, heartbeat_count = count);
    emit!(m, intern_used     = tracing::intern_count());
    emit!(m, time_ticks      = tracing::timestamp());
    // Histogram observation: how long the last IRQ took. The handler
    // measured rdtime delta; main thread emits.
    emit!(m, irq_duration    = trap::LAST_IRQ_DURATION.this_cpu().load(Ordering::Relaxed));
}

/// Frame allocator telemetry. Counters drain atomically; gauges
/// briefly take the allocator lock (heartbeat is single-threaded so
/// no contention).
fn emit_frame_metrics(m: &Metrics) {
    emit!(m, frames_allocated    = frame::ALLOC_COUNT.load(Ordering::Relaxed));
    emit!(m, frames_freed        = frame::FREE_COUNT.load(Ordering::Relaxed));
    emit!(m, frames_alloc_failed = frame::ALLOC_FAIL_COUNT.load(Ordering::Relaxed));
    if let Some(stats) = frame::stats() {
        emit!(m, frames_in_use = stats.in_use);
        emit!(m, frames_free   = stats.free);
    }
}

/// Kernel heap telemetry. Counters are atomics; the byte gauges come
/// from `heap::stats()`, which briefly takes the heap lock — safe
/// from the heartbeat (single-threaded, no contention with allocator
/// callers). `bytes_used` sums alignment-padded `layout.size()` for
/// live allocations; it excludes hole-list metadata, so it slightly
/// undercounts unavailable bytes.
///
/// Also drives the watermark-grow side effect: the policy (when + by
/// how much) is pure logic in `kernel_core::heap`; this fn owns the
/// side effect of acting on the decision. Heartbeat is
/// single-threaded so it's safe to take the heap lock for `extend`.
/// On failure (ceiling, OOM, map error) we bump `grow_failed_total`
/// and keep going — the next alloc fails with `alloc_failed_total`
/// as today.
fn emit_heap_metrics(m: &Metrics) {
    emit!(m, heap_alloc_total   = heap::ALLOC_COUNT.load(Ordering::Relaxed));
    emit!(m, heap_dealloc_total = heap::DEALLOC_COUNT.load(Ordering::Relaxed));
    emit!(m, heap_alloc_failed  = heap::ALLOC_FAIL_COUNT.load(Ordering::Relaxed));
    if let Some(hstats) = heap::stats() {
        emit!(m, heap_bytes_capacity     = hstats.capacity);
        emit!(m, heap_bytes_used         = hstats.used);
        emit!(m, heap_bytes_free         = hstats.free);
        emit!(m, heap_free_blocks        = hstats.free_blocks);
        emit!(m, heap_largest_free_block = hstats.largest_free_block);
        if let Some(frames) = heap::watermark_grow_decision(hstats, &heap::WATERMARK) {
            let _ = heap::extend(frames);
        }
    }
    emit!(m, heap_grow_total  = heap::GROW_COUNT.load(Ordering::Relaxed));
    emit!(m, heap_grow_failed = heap::GROW_FAIL_COUNT.load(Ordering::Relaxed));
}

fn emit_sched_metrics(m: &Metrics) {
    emit!(m, sched_smoke_marker_hits = sched::SMOKE_MARKER_HITS.load(Ordering::Relaxed));
    emit!(m, sched_exit_smoke_hits   = sched::EXIT_SMOKE_HITS.load(Ordering::Relaxed));
    emit!(m, sched_context_switches  = sched::CONTEXT_SWITCHES.load(Ordering::Relaxed));
    let sched_snap = sched::stats();
    emit!(m, sched_runqueue_depth = sched_snap.runqueue_depth);
    emit!(m, sched_tasks_total    = sched_snap.tasks_total);
    emit!(m, sched_yield_overhead = sched::LAST_YIELD_OVERHEAD_TICKS.load(Ordering::Relaxed));
    // Per-task metrics: gated off under `deflake-spawn-storm` because
    // that build uses sentinel StringIds for these (see
    // Task::new_bare) — emitting against id 0 would mis-tag whichever
    // name id 0 is.
    #[cfg(not(feature = "deflake-spawn-storm"))]
    for snap in sched::task_snapshots() {
        tracing::emit_metric(snap.cpu_time_metric, snap.cpu_time_ticks as i64);
        tracing::emit_metric(snap.runs_metric, snap.runs as i64);
    }
    emit!(m, task_a_loops = crate::demo_tasks::TASK_A_LOOPS.load(Ordering::Relaxed));
    emit!(m, task_b_loops = crate::demo_tasks::TASK_B_LOOPS.load(Ordering::Relaxed));
}

fn emit_workload_metrics(m: &Metrics) {
    emit!(m, workload_produced      = workload::SAMPLES_PRODUCED.load(Ordering::Relaxed));
    // Acquire, and read *before* histogram_sum(): pairs with the
    // consumer's Release on SAMPLES_CONSUMED (possibly on hart 1 under
    // the `workload=smp` selection). Observing consumed=V here guarantees the
    // subsequent histogram read sees every bin write that produced V,
    // so the emitted `histogram_sum >= consumed` oracle holds.
    emit!(m, workload_consumed      = workload::SAMPLES_CONSUMED.load(Ordering::Acquire));
    emit!(m, workload_histogram_sum = workload::histogram_sum());
    emit!(m, workload_lock_wait     = workload::LOCK_WAIT_TICKS_TOTAL.load(Ordering::Relaxed));
    emit!(m, workload_queue_depth   = workload::queue_depth());
}

fn emit_smp_metrics(m: &Metrics) {
    emit!(m, ipi_received            = ipi::RECEIVED_TOTAL.load(Ordering::Relaxed));
    emit!(m, smp_harts_total         = percpu::MAX_HARTS);
    emit!(m, smp_boot_hart_id        = BOOT_MHARTID.load(Ordering::Relaxed));
    emit!(m, smp_secondary_wfi       = secondary::SECONDARY_WFI_COUNT.load(Ordering::Relaxed));
    emit!(m, mmu_shootdowns_received = ipi::SHOOTDOWNS_RECEIVED_TOTAL.load(Ordering::Relaxed));
    emit!(m, mmu_shootdowns_sent     = mmu::SHOOTDOWNS_SENT_TOTAL.load(Ordering::Relaxed));
    emit!(m, smp_probe_ticks         = secondary::PROBE_TICKS.load(Ordering::Relaxed));
}

/// SMOKE TEST — remove with heap_smoke module.
fn emit_heap_smoke_metrics(m: &Metrics, count: i64) {
    heap_smoke::step(count);
    let sst = heap_smoke::stats();
    emit!(m, smoke_entries   = sst.entries);
    emit!(m, smoke_primes    = sst.primes);
    emit!(m, smoke_candidate = sst.candidate);
}

/// Deflake scenario triggers + counters. Each storm runs once on the
/// first heartbeat tick (blocks main until complete); subsequent
/// heartbeats re-emit the latest counter values for the integration
/// scenario to observe. All three blocks are independent feature
/// gates; in practice only one is active per build.
#[allow(unused_variables)]
fn emit_deflake_metrics(m: &Metrics, count: i64) {
    #[cfg(feature = "deflake-spawn-storm")]
    {
        if count == 1 {
            crate::deflake::spawn_storm::run();
        }
        emit!(m, spawn_storm_acks = crate::deflake::spawn_storm::ACK_COUNTER.load(Ordering::Relaxed));
    }
    #[cfg(feature = "deflake-ipi-pong")]
    {
        if count == 1 {
            crate::deflake::ipi_pong::run();
        }
        emit!(m, ipi_pong_sends = crate::deflake::ipi_pong::SENDS.load(Ordering::Relaxed));
    }
    #[cfg(feature = "deflake-shootdown-storm")]
    {
        if count == 1 {
            crate::deflake::shootdown::run();
        }
        emit!(m, shootdown_storm_sends = crate::deflake::shootdown::SENDS.load(Ordering::Relaxed));
    }
    #[cfg(feature = "deflake-mutex-storm")]
    {
        // No `run()` call here — the storm bodies are spawned as
        // proper tasks from kmain; main's only job is to keep
        // emitting metrics so the harness can observe progress.
        emit!(m, mutex_storm_acquires_hart0 = crate::deflake::mutex_storm::ACQUIRES_HART0.load(Ordering::Relaxed));
        emit!(m, mutex_storm_acquires_hart1 = crate::deflake::mutex_storm::ACQUIRES_HART1.load(Ordering::Relaxed));
    }
    #[cfg(feature = "deflake-virtio-storm")]
    {
        emit!(m, virtio_storm_hart0_emits      = crate::deflake::virtio_storm::HART0_EMITS.load(Ordering::Relaxed));
        emit!(m, virtio_storm_hart1_iterations = crate::deflake::virtio_storm::HART1_ITERATIONS.load(Ordering::Relaxed));
    }
}
