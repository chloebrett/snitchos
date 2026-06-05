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

const BATCH: usize = 64;
const PRODUCER_SEED: u64 = 0xc0ffee_dead_beef;

static QUEUE: Mutex<Option<VecDeque<u64>>> = Mutex::new(None);
static HISTOGRAM: [AtomicU64; BUCKETS] = [const { AtomicU64::new(0) }; BUCKETS];

pub static SAMPLES_PRODUCED: AtomicU64 = AtomicU64::new(0);
pub static SAMPLES_CONSUMED: AtomicU64 = AtomicU64::new(0);

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
            {
                let mut guard = QUEUE.lock();
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
            let n = {
                let mut guard = QUEUE.lock();
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
