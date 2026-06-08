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

use heapless::spsc::{Consumer, Producer, Queue};
use kernel_core::batch_ring::BatchRing;
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

/// In-flight samples = produced − consumed. Equals the queue length
/// by construction (every produced sample is queued until consumed),
/// works for both the `Mutex` and SPSC variants, and takes **no lock**
/// — so the heartbeat reading this doesn't become a third contender on
/// the `Mutex` queue (which would inflate the very lock-wait we're
/// measuring). Bounded by `QUEUE_CAP`.
pub fn queue_depth() -> usize {
    let produced = SAMPLES_PRODUCED.load(Ordering::Relaxed);
    let consumed = SAMPLES_CONSUMED.load(Ordering::Relaxed);
    usize::try_from(produced.saturating_sub(consumed)).unwrap_or(usize::MAX)
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

// ---- Lock-free SPSC variant (`workload=smp-spsc`, v0.6 step 12) ----
//
// Same producer/consumer/histogram and the same `burst=N` knob, but the
// queue is a lock-free `heapless::spsc` ring instead of
// `Mutex<VecDeque>`. The hot path never takes a lock, so
// `LOCK_WAIT_TICKS_TOTAL` stays 0 — that's the "chokepoint removed" the
// measurement and the lock-wait Grafana panel show. Capacity matches
// `QUEUE_CAP` (heapless `Queue<T, N>` holds N-1 items).

const SPSC_N: usize = QUEUE_CAP + 1;

/// The SPSC ring. `static mut` because `split` needs a `&'static mut`
/// once at init; thereafter the `Producer`/`Consumer` endpoints own
/// disjoint ends and synchronise via the ring's internal atomics —
/// safe for one producer (hart 0) + one consumer (hart 1), which is
/// exactly what SPSC is for.
static mut SPSC_QUEUE: Queue<u64, SPSC_N> = Queue::new();

/// One-time handoff slots: `init_spsc` (hart 0, boot) puts the split
/// endpoints here; each task `take`s its endpoint once at entry, then
/// owns it and runs lock-free. The `Mutex` guards only the handoff.
static SPSC_PRODUCER: Mutex<Option<Producer<'static, u64, SPSC_N>>> = Mutex::new(None);
static SPSC_CONSUMER: Mutex<Option<Consumer<'static, u64, SPSC_N>>> = Mutex::new(None);

/// Split the SPSC ring and stash its endpoints. Call once from `kmain`
/// before spawning the spsc tasks.
pub fn init_spsc() {
    // SAFETY: called exactly once at boot, on hart 0, before the spsc
    // producer/consumer tasks are spawned — so this `&mut` to the static
    // is unique and there are no concurrent users yet.
    #[allow(
        clippy::deref_addrof,
        reason = "&mut *(&raw mut STATIC) is the required idiom for a unique &mut to a static; \
                  clippy's deref_addrof autofix would rewrite it to the forbidden &mut STATIC"
    )]
    let queue: &'static mut Queue<u64, SPSC_N> = unsafe { &mut *(&raw mut SPSC_QUEUE) };
    let (producer, consumer) = queue.split();
    *SPSC_PRODUCER.lock() = Some(producer);
    *SPSC_CONSUMER.lock() = Some(consumer);
}

pub extern "C" fn spsc_producer_entry() -> ! {
    let mut producer = SPSC_PRODUCER
        .lock()
        .take()
        .expect("init_spsc must run before spsc_producer is scheduled");
    let mut lcg = Lcg::new(PRODUCER_SEED);
    loop {
        let burst = BURST.load(Ordering::Relaxed);
        crate::span!("workload.produce");
        for _ in 0..burst {
            let mut batch = [0u64; BATCH];
            for slot in &mut batch {
                *slot = lcg.next();
            }
            let mut pushed = 0u64;
            for &s in &batch {
                // Lock-free enqueue. `Err` = ring full → backpressure;
                // drop this sample. `produced` counts what was actually
                // enqueued, so `produced − consumed` stays the depth.
                if producer.enqueue(s).is_ok() {
                    pushed += 1;
                }
            }
            SAMPLES_PRODUCED.fetch_add(pushed, Ordering::Relaxed);
        }
        sched::yield_now();
    }
}

pub extern "C" fn spsc_consumer_entry() -> ! {
    let mut consumer = SPSC_CONSUMER
        .lock()
        .take()
        .expect("init_spsc must run before spsc_consumer is scheduled");
    loop {
        let burst = BURST.load(Ordering::Relaxed);
        crate::span!("workload.consume");
        for _ in 0..burst {
            let mut n = 0u64;
            for _ in 0..BATCH {
                match consumer.dequeue() {
                    Some(s) => {
                        HISTOGRAM[bin_of(s, BUCKETS)].fetch_add(1, Ordering::Relaxed);
                        n += 1;
                    }
                    None => break,
                }
            }
            // Release: same cross-hart publish as the Mutex consumer.
            SAMPLES_CONSUMED.fetch_add(n, Ordering::Release);
        }
        sched::yield_now();
    }
}

// ---- Batched lock-free SPSC (`workload=smp-spsc-batch`) ----
//
// The controlled third variant: a lock-free ring that fences once *per
// batch* (like the Mutex's per-batch lock) instead of per item (like
// heapless). Isolates whether the high-burst SPSC slowdown is the
// per-item Release fences. `BatchRing` is `Sync`, so both tasks share
// the static directly — no split/handoff. Lock-free → LOCK_WAIT stays 0.

static BATCH_RING: BatchRing<QUEUE_CAP> = BatchRing::new();

pub extern "C" fn spsc_batch_producer_entry() -> ! {
    let mut lcg = Lcg::new(PRODUCER_SEED);
    loop {
        let burst = BURST.load(Ordering::Relaxed);
        crate::span!("workload.produce");
        for _ in 0..burst {
            let mut batch = [0u64; BATCH];
            for slot in &mut batch {
                *slot = lcg.next();
            }
            // One Release publishes the whole 64-item batch.
            let pushed = BATCH_RING.enqueue_batch(&batch) as u64;
            SAMPLES_PRODUCED.fetch_add(pushed, Ordering::Relaxed);
        }
        sched::yield_now();
    }
}

pub extern "C" fn spsc_batch_consumer_entry() -> ! {
    loop {
        let burst = BURST.load(Ordering::Relaxed);
        crate::span!("workload.consume");
        for _ in 0..burst {
            let mut buf = [0u64; BATCH];
            let n = BATCH_RING.dequeue_batch(&mut buf);
            for &s in &buf[..n] {
                HISTOGRAM[bin_of(s, BUCKETS)].fetch_add(1, Ordering::Relaxed);
            }
            SAMPLES_CONSUMED.fetch_add(n as u64, Ordering::Release);
        }
        sched::yield_now();
    }
}
