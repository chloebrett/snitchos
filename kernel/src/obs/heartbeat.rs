//! Kernel heartbeat. The boot path's `kmain` calls [`run`] once
//! everything else is up; from there main thread becomes a tick-driven
//! metrics emitter. Each tick opens a `kernel.heartbeat` span, runs
//! the per-tick smoke patterns (frame + heap exercise, see feature
//! flags), and emits every metric registered in [`Metrics`].
//!
//! The metric set is built with the `define_metrics!` macro: one
//! list, one line per metric, with optional `#[cfg]` per line. The
//! macro generates both the struct declaration and the
//! `register()` constructor, so adding or removing a metric is a
//! one-line edit. Inside [`run`], the `emit!` macro wraps the
//! `tracing::emit_metric(..., expr as i64)` call so each emission
//! is one line.

use core::sync::atomic::{AtomicU64, Ordering};

use protocol::StringId;

use crate::{boot_workload, frame, heap, heap_smoke, percpu, sched, span, tracing, trap, workload};

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
    ($m:expr, $field:ident = $value:expr) => {{
        #[allow(
            clippy::cast_lossless,
            reason = "generic over metric integer widths — call sites pass u64/usize/i64 \
                      where i64::from doesn't apply; the `as i64` is the one cast that fits all"
        )]
        let v = $value as i64;
        tracing::emit_metric($m.$field, v)
    }};
}

/// mhartid `OpenSBI` handed kmain as `_hart_id`. Captured at the top of
/// the bring-up block; drained by the heartbeat as
/// `snitchos.smp.boot_hart_id`. Useful because `OpenSBI`'s choice of
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
    counter   intern_released           = "snitchos.intern.strings_released_total";
    gauge     time_ticks                = "snitchos.time.ticks";
    histogram irq_duration              = "snitchos.irq.timer.duration_ticks";
    // frame allocator (the allocated/freed/alloc_failed counters are now
    // `DeferredCounter`s drained via `counter::drain_all`)
    gauge     frames_in_use             = "snitchos.frames.in_use";
    gauge     frames_free               = "snitchos.frames.free";
    // kernel heap — alloc/dealloc/alloc_failed/grow/grow_failed counters live
    // in the `DeferredCounter` registry now; these are the sampled gauges.
    gauge     heap_bytes_capacity       = "snitchos.heap.bytes_capacity";
    gauge     heap_bytes_used           = "snitchos.heap.bytes_used";
    gauge     heap_bytes_free           = "snitchos.heap.bytes_free";
    gauge     heap_free_blocks          = "snitchos.heap.free_blocks";
    gauge     heap_largest_free_block   = "snitchos.heap.largest_free_block_bytes";
    // scheduler — smoke/exit/wake/context_switches/preemptions (+ ipc + task
    // loops) are registry counters now; these are the sampled gauges/histogram.
    gauge     sched_runqueue_depth      = "snitchos.sched.runqueue_depth";
    gauge     sched_tasks_total         = "snitchos.sched.tasks_total";
    histogram sched_yield_overhead      = "snitchos.sched.yield_overhead_ticks";
    // workload — produced/lock_wait are registry counters; `consumed` stays
    // bespoke (its drain needs an `Acquire` load — the cross-hart oracle).
    counter   workload_consumed         = "snitchos.workload.samples_consumed_total";
    gauge     workload_histogram_sum    = "snitchos.workload.histogram_sum";
    gauge     workload_queue_depth      = "snitchos.workload.queue_depth";
    // SMP / IPI / MMU — received/secondary_wfi/shootdowns/probe_ticks are
    // registry counters now; these are the sampled gauges.
    gauge     smp_harts_total           = "snitchos.smp.harts_total";
    gauge     smp_boot_hart_id          = "snitchos.smp.boot_hart_id";
    // Fragmentation-workload metrics — the sawtooth these trace is what gives
    // heap.free_blocks / largest_free_block_bytes above something to show.
    gauge     smoke_entries             = "snitchos.heap_smoke.entries";
    gauge     smoke_primes              = "snitchos.heap_smoke.primes";
    gauge     smoke_candidate           = "snitchos.heap_smoke.candidate";
    // Storm scenario metrics — present only in `itest-workloads`
    // builds. The metric *names* keep the historical `deflake.`
    // namespace so existing itest baselines/dashboards stay valid.
    #[cfg(feature = "itest-workloads")]
    counter   spawn_storm_acks          = "snitchos.deflake.spawn_storm_acks";
    #[cfg(feature = "itest-workloads")]
    counter   ipi_pong_sends            = "snitchos.deflake.ipi_pong_sends";
    #[cfg(feature = "itest-workloads")]
    counter   shootdown_storm_sends     = "snitchos.deflake.shootdown_storm_sends";
    #[cfg(feature = "itest-workloads")]
    counter   mutex_storm_acquires_hart0 = "snitchos.deflake.mutex_storm_acquires_hart0";
    #[cfg(feature = "itest-workloads")]
    counter   mutex_storm_acquires_hart1 = "snitchos.deflake.mutex_storm_acquires_hart1";
    #[cfg(feature = "itest-workloads")]
    counter   virtio_storm_hart0_emits  = "snitchos.deflake.virtio_storm_hart0_emits";
    #[cfg(feature = "itest-workloads")]
    counter   virtio_storm_hart1_iterations = "snitchos.deflake.virtio_storm_hart1_iterations";
    // TLB-shootdown correctness oracle (itest-workloads only).
    #[cfg(feature = "itest-workloads")]
    counter   tlb_remap_rounds          = "snitchos.smp.tlb_remap_rounds";
    #[cfg(feature = "itest-workloads")]
    counter   tlb_stale_reads           = "snitchos.smp.tlb_stale_reads";
    #[cfg(feature = "itest-workloads")]
    counter   ping_turns                = "snitchos.smp.ping_turns_total";
    #[cfg(feature = "itest-workloads")]
    counter   pong_turns                = "snitchos.smp.pong_turns_total";
}

/// Heartbeat main loop. Never returns. Waits for the timer-IRQ
/// handler to flip `TICK_PENDING`; on each tick opens a span, runs
/// the smoke patterns, emits every metric, then yields back to the
/// scheduler.
#[allow(
    clippy::needless_pass_by_value,
    reason = "`run` is `-> !` and is the terminal owner of the registered Metrics \
              (see the handoff comment at the kmain call site); by-value is the honest signature"
)]
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
            crate::ramfb::present();
            emit_core(&metrics, count);
            crate::counter::drain_all();
            emit_frame_metrics(&metrics);
            emit_heap_metrics(&metrics);
            emit_sched_metrics(&metrics);
            emit_workload_metrics(&metrics);
            emit_smp_metrics(&metrics);
            emit_heap_smoke_metrics(&metrics, count);
            #[cfg(feature = "itest-workloads")]
            emit_storm_metrics(&metrics, count);
        }
        // Reclaim fire-and-forget kernel tasks that have exited — drops their
        // `Box<Task>` and returns each stack slot to the mapped pool. In the
        // heartbeat (not the switch path); the pool makes the drop shootdown-free.
        sched::reap_ownerless_exited();
        sched::yield_now();
    }
}

/// Smoke pattern that exercises the frame allocator each heartbeat.
/// Default: alloc+free, keeps `in_use` bounded. Under the `workload=frame-oom`
/// selection: leak **a quarter of the pool per tick**, so exhaustion is a *gradual*
/// ~4-heartbeat drain under sustained pressure — not a one-shot OOM (the trivial
/// case). The rate is pool-relative (`total / 4`) rather than a fixed count so the
/// gradual ~4-heartbeat shape holds **at any RAM size** — the point being that
/// `SnitchOS`'s OOM path works regardless of physical RAM, so `frame-oom` boots on a
/// deliberately small machine (snemu 48 MiB; see `xtask` `ram_mb_for`) while QEMU
/// and the default run 128 MiB, and `total/4` is exactly the old `8192/tick` at
/// 128 MiB. Drives the `frame-allocator-oom` scenario; `frame-oom` also runs on
/// the light spawn layout (no demo `task_a/task_b` burning between ticks; see
/// `kmain`).
fn frame_smoke() {
    use kernel_boot::bootargs::WorkloadKind;
    if boot_workload::selected() == Some(WorkloadKind::FrameOom) {
        let per_tick = frame::stats().map_or(8192, |s| s.total / 4);
        for _ in 0..per_tick {
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
    use kernel_boot::bootargs::WorkloadKind;
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
    emit!(m, intern_released = tracing::strings_released_total() as i64);
    emit!(m, time_ticks      = tracing::timestamp());
    // Histogram observation: how long the last IRQ took. The handler
    // measured rdtime delta; main thread emits.
    emit!(m, irq_duration    = trap::LAST_IRQ_DURATION.this_cpu().load(Ordering::Relaxed));
}

/// Frame allocator telemetry. Counters drain atomically; gauges
/// briefly take the allocator lock (heartbeat is single-threaded so
/// no contention).
fn emit_frame_metrics(m: &Metrics) {
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
/// how much) is pure logic in `kernel_mem::heap`; this fn owns the
/// side effect of acting on the decision. Heartbeat is
/// single-threaded so it's safe to take the heap lock for `extend`.
/// On failure (ceiling, OOM, map error) we bump `grow_failed_total`
/// and keep going — the next alloc fails with `alloc_failed_total`
/// as today.
fn emit_heap_metrics(m: &Metrics) {
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
}

fn emit_sched_metrics(m: &Metrics) {
    let sched_snap = sched::stats();
    emit!(m, sched_runqueue_depth = sched_snap.runqueue_depth);
    emit!(m, sched_tasks_total    = sched_snap.tasks_total);
    emit!(m, sched_yield_overhead = sched::LAST_YIELD_OVERHEAD_TICKS.load(Ordering::Relaxed));
    // Per-task metrics: skipped under the spawn storm and live-tasks workloads,
    // which use sentinel StringIds for these (see Task::new_bare) — emitting
    // against id 0 would mis-tag whichever name id 0 is.
    if !matches!(
        boot_workload::selected(),
        Some(
            kernel_boot::bootargs::WorkloadKind::SpawnStorm
                | kernel_boot::bootargs::WorkloadKind::LiveTasks
        )
    ) {
        for snap in sched::task_snapshots() {
            tracing::emit_metric(snap.cpu_time_metric, snap.cpu_time_ticks as i64);
            tracing::emit_metric(snap.runs_metric, snap.runs as i64);
            tracing::emit_metric(snap.stack_high_water_metric, snap.stack_high_water_bytes as i64);
        }
    }
}

fn emit_workload_metrics(m: &Metrics) {
    // Acquire, and read *before* histogram_sum(): pairs with the
    // consumer's Release on SAMPLES_CONSUMED (possibly on hart 1 under
    // the `workload=smp` selection). Observing consumed=V here guarantees the
    // subsequent histogram read sees every bin write that produced V,
    // so the emitted `histogram_sum >= consumed` oracle holds. (This is why
    // `samples_consumed` stays bespoke rather than a `DeferredCounter`.)
    emit!(m, workload_consumed      = workload::SAMPLES_CONSUMED.load(Ordering::Acquire));
    emit!(m, workload_histogram_sum = workload::histogram_sum());
    emit!(m, workload_queue_depth   = workload::queue_depth());
}

fn emit_smp_metrics(m: &Metrics) {
    emit!(m, smp_harts_total  = percpu::MAX_HARTS);
    emit!(m, smp_boot_hart_id = BOOT_MHARTID.load(Ordering::Relaxed));
}

/// Steps the fragmentation workload and emits its gauges. Runs on every
/// workload — the churn is what keeps the heap's free-block gauges alive.
fn emit_heap_smoke_metrics(m: &Metrics, count: i64) {
    heap_smoke::step(count);
    let sst = heap_smoke::stats();
    emit!(m, smoke_entries   = sst.entries);
    emit!(m, smoke_primes    = sst.primes);
    emit!(m, smoke_candidate = sst.candidate);
}

/// Storm scenario triggers + counters (`itest-workloads` only). The
/// heartbeat-driven storms (`spawn`/`ipi`/`shootdown`) run once on the
/// first tick (blocking main until complete); the task-driven storms
/// (`mutex`/`virtio`) run in their own spawned tasks, so here we only
/// re-emit their progress counters. At most one storm is selected per
/// boot.
#[cfg(feature = "itest-workloads")]
fn emit_storm_metrics(m: &Metrics, count: i64) {
    use kernel_boot::bootargs::WorkloadKind;
    // Only the storm workloads emit here; every other selection (default /
    // Smp* / OOM / userspace / IPC / FS) contributes no storm metrics.
    // `WorkloadKind::is_storm` (host-tested in kernel-boot) is the single
    // source of truth, so a new *non-storm* workload needs no change here.
    let Some(kind) = boot_workload::selected().filter(|k| k.is_storm()) else {
        return;
    };
    match kind {
        WorkloadKind::SpawnStorm => {
            if count == 1 {
                crate::storms::spawn_storm::run();
            }
            emit!(m, spawn_storm_acks = crate::storms::spawn_storm::ACK_COUNTER.load(Ordering::Relaxed));
        }
        WorkloadKind::IpiPong => {
            if count == 1 {
                crate::storms::ipi_pong::run();
            }
            emit!(m, ipi_pong_sends = crate::storms::ipi_pong::SENDS.load(Ordering::Relaxed));
        }
        WorkloadKind::ShootdownStorm => {
            if count == 1 {
                crate::storms::shootdown::run();
            }
            emit!(m, shootdown_storm_sends = crate::storms::shootdown::SENDS.load(Ordering::Relaxed));
        }
        WorkloadKind::MutexStorm => {
            emit!(m, mutex_storm_acquires_hart0 = crate::storms::mutex_storm::ACQUIRES_HART0.load(Ordering::Relaxed));
            emit!(m, mutex_storm_acquires_hart1 = crate::storms::mutex_storm::ACQUIRES_HART1.load(Ordering::Relaxed));
        }
        WorkloadKind::VirtioStorm => {
            emit!(m, virtio_storm_hart0_emits      = crate::storms::virtio_storm::HART0_EMITS.load(Ordering::Relaxed));
            emit!(m, virtio_storm_hart1_iterations = crate::storms::virtio_storm::HART1_ITERATIONS.load(Ordering::Relaxed));
        }
        WorkloadKind::TlbShootdown => {
            if count == 1 {
                crate::storms::tlb_shootdown::run();
            }
            emit!(m, tlb_remap_rounds = crate::storms::tlb_shootdown::ROUNDS.load(Ordering::Relaxed));
            emit!(m, tlb_stale_reads  = crate::storms::tlb_shootdown::STALE_READS.load(Ordering::Relaxed));
        }
        WorkloadKind::PingPong => {
            if count == 1 {
                crate::storms::ping_pong::run();
            }
            emit!(m, ping_turns = crate::storms::ping_pong::PING_TURNS.load(Ordering::Relaxed));
            emit!(m, pong_turns = crate::storms::ping_pong::PONG_TURNS.load(Ordering::Relaxed));
        }
        // Only storms reach here (gated by `is_storm` above). A new storm
        // variant without an arm trips this in debug builds (the itest profile).
        _ => debug_assert!(false, "is_storm() but no emit_storm_metrics arm: {kind:?}"),
    }
}
