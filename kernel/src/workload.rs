//! v0.6 step 1 — cooperative producer/consumer histogram.
//!
//! Producer task generates LCG samples in batches; consumer task
//! drains them under `kernel::sync::Mutex` and bins them into a
//! `[AtomicU64; BUCKETS]`. Both yield after each batch so the
//! cooperative scheduler can interleave them.
//!
//! Pure logic (`Lcg`, `bin_of`, `bin_sample`) lives in
//! `kernel_core::workload` and is host-tested. This file wires those
//! primitives to the scheduler, to `kernel::sync::Mutex`, and to the
//! atomic-backed counters the heartbeat drains.
//!
//! Invariant: every sample the consumer pulls from the queue is binned
//! exactly once. Therefore `histogram_sum() >= SAMPLES_CONSUMED` at all
//! times — equality at heartbeat-sample boundaries, with at most one
//! batch's lag if sampled mid-consume. The
//! `workload-cooperative-baseline` integration scenario asserts this.

use alloc::collections::VecDeque;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use kernel_core::workload::{bin_of, Lcg, BUCKETS};

use crate::sched;
use crate::sync::Mutex;
use crate::tracing;

const BATCH: usize = 64;
const PRODUCER_SEED: u64 = 0x00c0_ffee_dead_beef;

/// Bounded queue capacity (matches the planned SPSC ring size so the
/// Mutex and SPSC variants are an apples-to-apples comparison). When
/// full, the producer's push is a no-op but still takes the lock — so
/// backpressure shows up as lock traffic, not unbounded memory growth.
const QUEUE_CAP: usize = 4096;

/// Batches each task runs per `yield_now`. Default 1 (one batch then
/// yield — the cooperative, low-contention shape). A larger burst keeps
/// each hart in its loop long enough that producer (hart 0) and
/// consumer (hart 1) overlap and actually contend on `QUEUE`. Set at
/// boot from the `burst=N` kernel bootarg via `set_burst`. See
/// `docs/v0.6-mutex-vs-spsc-measurements.md`.
static BURST: AtomicUsize = AtomicUsize::new(1);

/// Set the per-yield burst length (clamped to ≥ 1). Called once from
/// `kmain` when a `burst=N` bootarg is present.
pub fn set_burst(n: usize) {
    BURST.store(n.max(1), Ordering::Relaxed);
}

static QUEUE: Mutex<Option<VecDeque<u64>>> = Mutex::new(None);

// `Relaxed` on the per-bin tallies and on PRODUCED/LOCK_WAIT: each is a
// pure counter and inter-bin consistency isn't load-bearing.
//
// SAMPLES_CONSUMED is the exception. The consumer bins a batch
// (HISTOGRAM, Relaxed) and *then* bumps SAMPLES_CONSUMED — and under
// the `workload=smp` selection the consumer runs on hart 1 while the
// heartbeat reads both counters on hart 0. To keep the correctness oracle
// `histogram_sum >= SAMPLES_CONSUMED` valid across that hart boundary,
// the SAMPLES_CONSUMED bump is a `Release` and the heartbeat's read is
// the matching `Acquire` (see `heartbeat::emit_workload_metrics`): an
// Acquire-load that observes a consumed value V is guaranteed to see
// every bin write sequenced before that Release, so a subsequent
// histogram read yields `>= V`. With plain Relaxed, hart 0 could
// observe the consumed bump ahead of the bin writes and the oracle
// would spuriously fail. See `kernel::percpu` for the kernel-wide
// ordering discipline.
static HISTOGRAM: [AtomicU64; BUCKETS] = [const { AtomicU64::new(0) }; BUCKETS];

pub static SAMPLES_PRODUCED: AtomicU64 = AtomicU64::new(0);
pub static SAMPLES_CONSUMED: AtomicU64 = AtomicU64::new(0);

/// Cumulative ticks spent waiting to acquire `QUEUE` across all
/// producer and consumer lock acquisitions. Today's cooperative
/// single-hart kernel sees ~0 wait (no contender can run while we're
/// trying to lock); the value will become interesting in v0.6 step 11
/// when producer + consumer run on different harts and the lock
/// cacheline starts ping-ponging.
pub static LOCK_WAIT_TICKS_TOTAL: AtomicU64 = AtomicU64::new(0);

/// Sum across all histogram bins at the moment of the call. The
/// individual `fetch_add` operations are `Relaxed` (each bin is its own
/// counter; bins don't synchronise with each other), so this sum may
/// trail an in-flight `consume` batch by up to BATCH. That's fine for
/// the `>= SAMPLES_CONSUMED` invariant — observed by the heartbeat,
/// which runs on the same hart as the consumer in v0.6 step 1 and
/// only races with it at yield boundaries.
pub fn histogram_sum() -> u64 {
    HISTOGRAM.iter().map(|b| b.load(Ordering::Relaxed)).sum()
}

/// Current queue depth. Briefly takes the queue lock — safe to call
/// from the heartbeat (single-threaded vs. producer/consumer in v0.6
/// step 1; under SMP this is one more lock contender but still bounded).
/// Returns 0 if the queue hasn't been initialised yet (first call into
/// producer/consumer not happened).
pub fn queue_depth() -> usize {
    let guard = QUEUE.lock();
    guard.as_ref().map(|q| q.len()).unwrap_or(0)
}

pub extern "C" fn producer_entry() -> ! {
    let mut lcg = Lcg::new(PRODUCER_SEED);
    loop {
        let burst = BURST.load(Ordering::Relaxed);
        // One span per burst, not per batch — per-batch spans would
        // flood the wire (and their virtio MMIO would dwarf the lock
        // traffic we're trying to measure).
        crate::span!("workload.produce");
        for _ in 0..burst {
            // Generate a batch off-lock so the lock critical section
            // is just the queue push, not the LCG work.
            let mut batch = [0u64; BATCH];
            for slot in &mut batch {
                *slot = lcg.next();
            }
            let t_lock_start = tracing::timestamp();
            let pushed = {
                let mut guard = QUEUE.lock();
                let wait = tracing::timestamp().saturating_sub(t_lock_start);
                LOCK_WAIT_TICKS_TOTAL.fetch_add(wait, Ordering::Relaxed);
                let q = guard.get_or_insert_with(VecDeque::new);
                let room = QUEUE_CAP.saturating_sub(q.len());
                let take = BATCH.min(room);
                for &s in batch.iter().take(take) {
                    q.push_back(s);
                }
                take
            };
            SAMPLES_PRODUCED.fetch_add(pushed as u64, Ordering::Relaxed);
        }
        sched::yield_now();
    }
}

pub extern "C" fn consumer_entry() -> ! {
    loop {
        let burst = BURST.load(Ordering::Relaxed);
        crate::span!("workload.consume");
        for _ in 0..burst {
            // Drain a batch under the lock, bin off-lock.
            let mut buf = [0u64; BATCH];
            let t_lock_start = tracing::timestamp();
            let n = {
                let mut guard = QUEUE.lock();
                let wait = tracing::timestamp().saturating_sub(t_lock_start);
                LOCK_WAIT_TICKS_TOTAL.fetch_add(wait, Ordering::Relaxed);
                let q = guard.get_or_insert_with(VecDeque::new);
                let n = q.len().min(BATCH);
                for slot in buf.iter_mut().take(n) {
                    *slot = q.pop_front().expect("len bounded above");
                }
                n
            };
            for s in buf.iter().take(n) {
                let bin = bin_of(*s, BUCKETS);
                HISTOGRAM[bin].fetch_add(1, Ordering::Relaxed);
            }
            // Release: publishes the bin writes above to the hart 0
            // heartbeat's Acquire-load. See the SAMPLES_CONSUMED note
            // at the top of this module.
            SAMPLES_CONSUMED.fetch_add(n as u64, Ordering::Release);
        }
        sched::yield_now();
    }
}
