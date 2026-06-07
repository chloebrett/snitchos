//! Kernel heartbeat. The boot path's `kmain` calls [`run`] once
//! everything else is up; from there main thread becomes a tick-driven
//! metrics emitter. Each tick opens a `kernel.heartbeat` span, runs
//! the per-tick smoke patterns (frame + heap exercise, see feature
//! flags), and emits every metric registered in [`Metrics`].
//!
//! The metric set is a single struct so kmain holds one value instead
//! of 35+ locals. `Metrics::register` is the only side-effecting
//! constructor; reads + emits inside the tick are plain field
//! accesses.

use core::sync::atomic::{AtomicU64, Ordering};

use protocol::StringId;

use crate::{frame, heap, heap_smoke, ipi, mmu, percpu, sched, secondary, span, tracing, trap, workload};

/// mhartid OpenSBI handed kmain as `_hart_id`. Captured at the top of
/// the bring-up block; drained by the heartbeat as
/// `snitchos.smp.boot_hart_id`. Useful because OpenSBI's choice of
/// boot hart is not always 0 under `-smp 2`. `Relaxed`: single writer
/// (boot path), read by heartbeat on the same hart.
pub static BOOT_MHARTID: AtomicU64 = AtomicU64::new(0);

/// All `StringId`s for the metrics the heartbeat emits. Constructed
/// once via [`Metrics::register`] right before [`run`] takes over; the
/// register calls are idempotent so re-running them would be safe but
/// wasteful (each does a hash-table insert + frame emit).
pub struct Metrics {
    heartbeat_count: StringId,
    intern_used: StringId,
    time_ticks: StringId,
    irq_duration: StringId,
    // frame allocator
    frames_allocated: StringId,
    frames_freed: StringId,
    frames_alloc_failed: StringId,
    frames_in_use: StringId,
    frames_free: StringId,
    // kernel heap
    heap_alloc_total: StringId,
    heap_dealloc_total: StringId,
    heap_alloc_failed: StringId,
    heap_bytes_capacity: StringId,
    heap_bytes_used: StringId,
    heap_bytes_free: StringId,
    heap_grow_total: StringId,
    heap_grow_failed: StringId,
    heap_free_blocks: StringId,
    heap_largest_free_block: StringId,
    // scheduler
    sched_smoke_marker_hits: StringId,
    sched_context_switches: StringId,
    sched_runqueue_depth: StringId,
    sched_tasks_total: StringId,
    sched_yield_overhead: StringId,
    // demo tasks
    task_a_loops: StringId,
    task_b_loops: StringId,
    // workload
    workload_produced: StringId,
    workload_consumed: StringId,
    workload_histogram_sum: StringId,
    workload_lock_wait: StringId,
    workload_queue_depth: StringId,
    // SMP / IPI
    ipi_received: StringId,
    smp_harts_total: StringId,
    smp_boot_hart_id: StringId,
    smp_secondary_wfi: StringId,
    mmu_shootdowns_received: StringId,
    mmu_shootdowns_sent: StringId,
    smp_probe_ticks: StringId,
    // SMOKE TEST metrics — remove with heap_smoke module
    smoke_entries: StringId,
    smoke_primes: StringId,
    smoke_candidate: StringId,
    // deflake scenario metrics
    #[cfg(feature = "deflake-spawn-storm")]
    spawn_storm_acks: StringId,
    #[cfg(feature = "deflake-ipi-pong")]
    ipi_pong_sends: StringId,
    #[cfg(feature = "deflake-shootdown-storm")]
    shootdown_storm_sends: StringId,
}

impl Metrics {
    pub fn register() -> Self {
        Self {
            heartbeat_count: tracing::register_counter("snitchos.heartbeat.count"),
            intern_used: tracing::register_gauge("snitchos.intern.strings_used"),
            time_ticks: tracing::register_gauge("snitchos.time.ticks"),
            irq_duration: tracing::register_histogram("snitchos.irq.timer.duration_ticks"),
            frames_allocated: tracing::register_counter("snitchos.frames.allocated_total"),
            frames_freed: tracing::register_counter("snitchos.frames.freed_total"),
            frames_alloc_failed: tracing::register_counter("snitchos.frames.alloc_failed_total"),
            frames_in_use: tracing::register_gauge("snitchos.frames.in_use"),
            frames_free: tracing::register_gauge("snitchos.frames.free"),
            heap_alloc_total: tracing::register_counter("snitchos.heap.alloc_total"),
            heap_dealloc_total: tracing::register_counter("snitchos.heap.dealloc_total"),
            heap_alloc_failed: tracing::register_counter("snitchos.heap.alloc_failed_total"),
            heap_bytes_capacity: tracing::register_gauge("snitchos.heap.bytes_capacity"),
            heap_bytes_used: tracing::register_gauge("snitchos.heap.bytes_used"),
            heap_bytes_free: tracing::register_gauge("snitchos.heap.bytes_free"),
            heap_grow_total: tracing::register_counter("snitchos.heap.grow_total"),
            heap_grow_failed: tracing::register_counter("snitchos.heap.grow_failed_total"),
            heap_free_blocks: tracing::register_gauge("snitchos.heap.free_blocks"),
            heap_largest_free_block: tracing::register_gauge(
                "snitchos.heap.largest_free_block_bytes",
            ),
            sched_smoke_marker_hits: tracing::register_counter(
                "snitchos.sched.smoke_marker_hits",
            ),
            sched_context_switches: tracing::register_counter(
                "snitchos.sched.context_switches_total",
            ),
            sched_runqueue_depth: tracing::register_gauge("snitchos.sched.runqueue_depth"),
            sched_tasks_total: tracing::register_gauge("snitchos.sched.tasks_total"),
            sched_yield_overhead: tracing::register_histogram(
                "snitchos.sched.yield_overhead_ticks",
            ),
            task_a_loops: tracing::register_counter("snitchos.task_a.loops"),
            task_b_loops: tracing::register_counter("snitchos.task_b.loops"),
            workload_produced: tracing::register_counter(
                "snitchos.workload.samples_produced_total",
            ),
            workload_consumed: tracing::register_counter(
                "snitchos.workload.samples_consumed_total",
            ),
            workload_histogram_sum: tracing::register_gauge("snitchos.workload.histogram_sum"),
            workload_lock_wait: tracing::register_counter(
                "snitchos.workload.lock_wait_ticks_total",
            ),
            workload_queue_depth: tracing::register_gauge("snitchos.workload.queue_depth"),
            ipi_received: tracing::register_counter("snitchos.ipi.received_total"),
            smp_harts_total: tracing::register_gauge("snitchos.smp.harts_total"),
            smp_boot_hart_id: tracing::register_gauge("snitchos.smp.boot_hart_id"),
            smp_secondary_wfi: tracing::register_counter("snitchos.smp.secondary_wfi_total"),
            mmu_shootdowns_received: tracing::register_counter(
                "snitchos.mmu.shootdowns_received_total",
            ),
            mmu_shootdowns_sent: tracing::register_counter(
                "snitchos.mmu.shootdowns_sent_total",
            ),
            smp_probe_ticks: tracing::register_counter(
                "snitchos.smp.hart_1_probe_ticks_total",
            ),
            smoke_entries: tracing::register_gauge("snitchos.heap_smoke.entries"),
            smoke_primes: tracing::register_gauge("snitchos.heap_smoke.primes"),
            smoke_candidate: tracing::register_gauge("snitchos.heap_smoke.candidate"),
            #[cfg(feature = "deflake-spawn-storm")]
            spawn_storm_acks: tracing::register_counter(
                "snitchos.deflake.spawn_storm_acks",
            ),
            #[cfg(feature = "deflake-ipi-pong")]
            ipi_pong_sends: tracing::register_counter("snitchos.deflake.ipi_pong_sends"),
            #[cfg(feature = "deflake-shootdown-storm")]
            shootdown_storm_sends: tracing::register_counter(
                "snitchos.deflake.shootdown_storm_sends",
            ),
        }
    }
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
/// Default build: alloc+free, keeps `in_use` bounded. `oom-leak`
/// feature: leak 8192 frames per tick (32 MiB) so the allocator's
/// ~32K-frame free pool exhausts in ~4 heartbeats. Used by the
/// `frame-allocator-oom` integration scenario.
fn frame_smoke() {
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
}

/// Heap smoke. Default build: alloc + write + drop a 256 B Vec —
/// proves the heap is live, keeps `bytes_used` near 0 across
/// heartbeats. `heap-oom` feature: per-heartbeat leak loop using the
/// raw `GlobalAlloc` API (returns null on failure rather than
/// panicking through `alloc_error_handler`). After the heap exhausts,
/// every subsequent iteration's first allocation returns null
/// immediately and bumps `alloc_failed_total` — so the counter climbs
/// once per heartbeat post-OOM, and the kernel keeps heartbeating.
fn heap_smoke_pattern(count: i64) {
    #[cfg(not(feature = "heap-oom"))]
    {
        let mut v: alloc::vec::Vec<u8> = alloc::vec::Vec::with_capacity(256);
        v.push(count as u8);
    }
    #[cfg(feature = "heap-oom")]
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
    tracing::emit_metric(m.heartbeat_count, count);
    tracing::emit_metric(m.intern_used, tracing::intern_count() as i64);
    tracing::emit_metric(m.time_ticks, tracing::timestamp() as i64);
    // Histogram observation: how long the last IRQ took. The handler
    // measured rdtime delta; main thread emits.
    let dur = trap::LAST_IRQ_DURATION.this_cpu().load(Ordering::Relaxed);
    tracing::emit_metric(m.irq_duration, dur as i64);
}

/// Frame allocator telemetry. Counters drain atomically; gauges
/// briefly take the allocator lock (heartbeat is single-threaded so
/// no contention).
fn emit_frame_metrics(m: &Metrics) {
    tracing::emit_metric(
        m.frames_allocated,
        frame::ALLOC_COUNT.load(Ordering::Relaxed) as i64,
    );
    tracing::emit_metric(
        m.frames_freed,
        frame::FREE_COUNT.load(Ordering::Relaxed) as i64,
    );
    tracing::emit_metric(
        m.frames_alloc_failed,
        frame::ALLOC_FAIL_COUNT.load(Ordering::Relaxed) as i64,
    );
    if let Some(stats) = frame::stats() {
        tracing::emit_metric(m.frames_in_use, stats.in_use as i64);
        tracing::emit_metric(m.frames_free, stats.free as i64);
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
    tracing::emit_metric(
        m.heap_alloc_total,
        heap::ALLOC_COUNT.load(Ordering::Relaxed) as i64,
    );
    tracing::emit_metric(
        m.heap_dealloc_total,
        heap::DEALLOC_COUNT.load(Ordering::Relaxed) as i64,
    );
    tracing::emit_metric(
        m.heap_alloc_failed,
        heap::ALLOC_FAIL_COUNT.load(Ordering::Relaxed) as i64,
    );
    if let Some(hstats) = heap::stats() {
        tracing::emit_metric(m.heap_bytes_capacity, hstats.capacity as i64);
        tracing::emit_metric(m.heap_bytes_used, hstats.used as i64);
        tracing::emit_metric(m.heap_bytes_free, hstats.free as i64);
        tracing::emit_metric(m.heap_free_blocks, hstats.free_blocks as i64);
        tracing::emit_metric(m.heap_largest_free_block, hstats.largest_free_block as i64);
        if let Some(frames) = heap::watermark_grow_decision(hstats, &heap::WATERMARK) {
            let _ = heap::extend(frames);
        }
    }
    tracing::emit_metric(
        m.heap_grow_total,
        heap::GROW_COUNT.load(Ordering::Relaxed) as i64,
    );
    tracing::emit_metric(
        m.heap_grow_failed,
        heap::GROW_FAIL_COUNT.load(Ordering::Relaxed) as i64,
    );
}

fn emit_sched_metrics(m: &Metrics) {
    tracing::emit_metric(
        m.sched_smoke_marker_hits,
        sched::SMOKE_MARKER_HITS.load(Ordering::Relaxed) as i64,
    );
    tracing::emit_metric(
        m.sched_context_switches,
        sched::CONTEXT_SWITCHES.load(Ordering::Relaxed) as i64,
    );
    let sched_snap = sched::stats();
    tracing::emit_metric(m.sched_runqueue_depth, sched_snap.runqueue_depth as i64);
    tracing::emit_metric(m.sched_tasks_total, sched_snap.tasks_total as i64);
    tracing::emit_metric(
        m.sched_yield_overhead,
        sched::LAST_YIELD_OVERHEAD_TICKS.load(Ordering::Relaxed) as i64,
    );
    // Per-task metrics: gated off under `deflake-spawn-storm` because
    // that build uses sentinel StringIds for these (see
    // Task::new_bare) — emitting against id 0 would mis-tag whichever
    // name id 0 is.
    #[cfg(not(feature = "deflake-spawn-storm"))]
    for snap in sched::task_snapshots() {
        tracing::emit_metric(snap.cpu_time_metric, snap.cpu_time_ticks as i64);
        tracing::emit_metric(snap.runs_metric, snap.runs as i64);
    }
    tracing::emit_metric(
        m.task_a_loops,
        crate::demo_tasks::TASK_A_LOOPS.load(Ordering::Relaxed) as i64,
    );
    tracing::emit_metric(
        m.task_b_loops,
        crate::demo_tasks::TASK_B_LOOPS.load(Ordering::Relaxed) as i64,
    );
}

fn emit_workload_metrics(m: &Metrics) {
    tracing::emit_metric(
        m.workload_produced,
        workload::SAMPLES_PRODUCED.load(Ordering::Relaxed) as i64,
    );
    tracing::emit_metric(
        m.workload_consumed,
        workload::SAMPLES_CONSUMED.load(Ordering::Relaxed) as i64,
    );
    tracing::emit_metric(m.workload_histogram_sum, workload::histogram_sum() as i64);
    tracing::emit_metric(
        m.workload_lock_wait,
        workload::LOCK_WAIT_TICKS_TOTAL.load(Ordering::Relaxed) as i64,
    );
    tracing::emit_metric(m.workload_queue_depth, workload::queue_depth() as i64);
}

fn emit_smp_metrics(m: &Metrics) {
    tracing::emit_metric(
        m.ipi_received,
        ipi::RECEIVED_TOTAL.load(Ordering::Relaxed) as i64,
    );
    tracing::emit_metric(m.smp_harts_total, percpu::MAX_HARTS as i64);
    tracing::emit_metric(
        m.smp_boot_hart_id,
        BOOT_MHARTID.load(Ordering::Relaxed) as i64,
    );
    tracing::emit_metric(
        m.smp_secondary_wfi,
        secondary::SECONDARY_WFI_COUNT.load(Ordering::Relaxed) as i64,
    );
    tracing::emit_metric(
        m.mmu_shootdowns_received,
        ipi::SHOOTDOWNS_RECEIVED_TOTAL.load(Ordering::Relaxed) as i64,
    );
    tracing::emit_metric(
        m.mmu_shootdowns_sent,
        mmu::SHOOTDOWNS_SENT_TOTAL.load(Ordering::Relaxed) as i64,
    );
    tracing::emit_metric(
        m.smp_probe_ticks,
        secondary::PROBE_TICKS.load(Ordering::Relaxed) as i64,
    );
}

/// SMOKE TEST — remove with heap_smoke module.
fn emit_heap_smoke_metrics(m: &Metrics, count: i64) {
    heap_smoke::step(count);
    let sst = heap_smoke::stats();
    tracing::emit_metric(m.smoke_entries, sst.entries as i64);
    tracing::emit_metric(m.smoke_primes, sst.primes as i64);
    tracing::emit_metric(m.smoke_candidate, sst.candidate as i64);
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
        tracing::emit_metric(
            m.spawn_storm_acks,
            crate::deflake::spawn_storm::ACK_COUNTER.load(Ordering::Relaxed) as i64,
        );
    }
    #[cfg(feature = "deflake-ipi-pong")]
    {
        if count == 1 {
            crate::deflake::ipi_pong::run();
        }
        tracing::emit_metric(
            m.ipi_pong_sends,
            crate::deflake::ipi_pong::SENDS.load(Ordering::Relaxed) as i64,
        );
    }
    #[cfg(feature = "deflake-shootdown-storm")]
    {
        if count == 1 {
            crate::deflake::shootdown::run();
        }
        tracing::emit_metric(
            m.shootdown_storm_sends,
            crate::deflake::shootdown::SENDS.load(Ordering::Relaxed) as i64,
        );
    }
}
