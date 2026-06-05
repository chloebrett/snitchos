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
use core::sync::atomic::{AtomicU64, Ordering};

use kernel_core::workload::{bin_of, Lcg, BUCKETS};

use crate::sched;
use crate::sync::Mutex;
use crate::tracing;

const BATCH: usize = 64;
const PRODUCER_SEED: u64 = 0xc0ffee_dead_beef;

static QUEUE: Mutex<Option<VecDeque<u64>>> = Mutex::new(None);

// `Relaxed` everywhere on the workload atomics: each `fetch_add` is a
// pure counter (per-bin tally for HISTOGRAM, total counts for
// PRODUCED/CONSUMED/LOCK_WAIT). Inter-bin and inter-counter
// consistency is not load-bearing — the heartbeat drains them
// independently and the correctness oracle
// `histogram_sum >= SAMPLES_CONSUMED` holds at boundaries.
// See `kernel::percpu` for the kernel-wide ordering discipline.
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
        {
            crate::span!("workload.produce");
            // Generate a batch off-lock so the lock critical section
            // is just the queue push, not the LCG work.
            let mut batch = [0u64; BATCH];
            for slot in &mut batch {
                *slot = lcg.next();
            }
            let t_lock_start = tracing::timestamp();
            {
                let mut guard = QUEUE.lock();
                let wait = tracing::timestamp().saturating_sub(t_lock_start);
                LOCK_WAIT_TICKS_TOTAL.fetch_add(wait, Ordering::Relaxed);
                let q = guard.get_or_insert_with(VecDeque::new);
                for s in batch {
                    q.push_back(s);
                }
            }
            SAMPLES_PRODUCED.fetch_add(BATCH as u64, Ordering::Relaxed);
        }
        sched::yield_now();
    }
}

pub extern "C" fn consumer_entry() -> ! {
    loop {
        {
            crate::span!("workload.consume");
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
            SAMPLES_CONSUMED.fetch_add(n as u64, Ordering::Relaxed);
        }
        sched::yield_now();
    }
}
