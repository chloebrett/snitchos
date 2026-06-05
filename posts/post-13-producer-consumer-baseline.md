# Post 13 — Two threads, one queue, one CPU

> v0.6 step 1: a producer/consumer histogram workload running cooperatively on a single hart. The first instalment of a three-post arc that lands at SMP. This post is the baseline we'll measure the next two against.

## why we're doing this now

- v0.5 shipped: cooperative round-robin, four threads, span context that survives a context switch. The kernel is now organised around tasks.
- v0.6 is **SMP — cooperative**. Second hart online, per-CPU discipline made real, the `kernel::sync` + `PerCpu` chokepoints earn their keep under genuine multi-hart contention.
- but jumping straight to two harts with no workload is a thin post — "two heartbeats on two harts." The v0.5 prefactor positioned us perfectly; we'd be proving the discipline without anything to demonstrate it *on*.
- so the v0.6 arc is three posts: **this one (cooperative single-hart baseline)**, then SMP with `Mutex<VecDeque>` (the chokepoint shows its cost), then SMP with `heapless::spsc` (the chokepoint goes away). One workload, three configurations, three observability stories.

## the workload

- a producer task pulls LCG samples in batches of 64; a consumer task drains the queue and bins them into a 64-bucket histogram. Both tasks `yield_now()` after each batch so the cooperative scheduler interleaves them.
- the queue is `kernel::sync::Mutex<Option<VecDeque<u64>>>` — the *chokepoint exercises itself* on every batch. Lazy `Option<>` initialisation matches the existing `heap_smoke::TABLE` pattern; no const-fn gymnastics with `VecDeque::new()`.
- the histogram is `[AtomicU64; 64]` — no lock on the hot bin path. Producer/consumer compete for the queue, not for individual bins.
- pure logic lives in `kernel-core::workload`:
  - `Lcg::new(seed).next()` — Knuth's MMIX recurrence, same constants the v0.5 `burn_lcg` used but now we *keep* the value instead of `black_box`-ing it
  - `bin_of(sample, BUCKETS)` — pure mod, range-invariant pinned by host test
  - `bin_sample(&mut [u64; BUCKETS], sample)` — the correctness oracle: after N calls starting from zeros, sum of bins is exactly N
- 8 host tests for the pure layer, asserting determinism, distinctness, range, purity, and the histogram invariant. These are what the in-kernel atomic-backed version must replicate.

## what makes this an observability post and not a workload post

- the integration scenario asserts two things and we emit *five* metrics. The extra three are for the dashboard, not the test. Reasons:
  - `samples_produced_total` + `samples_consumed_total` — pair tracks each other on this hart, will diverge on SMP if anything goes wrong. Cumulative shape, easy to read in Grafana.
  - `histogram_sum` — the live version of the correctness oracle. Should always equal `samples_consumed_total` (lags by ≤ BATCH while the consumer is mid-bin). A "samples lost" panel makes the invariant a stat block with a colour threshold.
  - `lock_wait_ticks_total` — `rdtime` deltas around `QUEUE.lock()` acquires. On cooperative single hart this is ~0. **This metric is the headline of post 3.** We're emitting it now so the baseline-of-zero is captured before the chokepoint has anything to contend with.
  - `queue_depth` — sampled at heartbeat-time via a quick lock. On cooperative single hart, oscillates between 0 and one batch (consumer drains immediately after producer yields). The *shape* of this graph will change under SMP and again under SPSC.
- the scenario itself is small: `samples_consumed_total >= 500` within 15s (workload is alive) AND `histogram_sum >= consumed_observed` (no samples lost or duplicated). The mutants this kills:
  - "consumer drops samples" → histogram_sum lags consumed forever → fails the second assertion
  - "producer never runs" → consumed never moves → fails the first
  - "binning silently double-counts" → histogram_sum overshoots, but the assertion is `>=`, so misses this. Worth tightening later, perhaps as a strict equality at heartbeat boundaries.

## the dynamics you can actually see right now

- five-task round-robin: `main` (heartbeat), `idle`, `task_a` (300K LCG iters per loop), `task_b` (900K LCG iters per loop), `workload_producer`, `workload_consumer`.
- `task_a` and `task_b` dominate the CPU budget — that was the v0.5 demo, designed to make per-task `cpu_time_ticks` visible. The workload tasks fit into the remaining time and manage ~one batch per heartbeat. The `samples_consumed_total >= 500` threshold reflects that.
- when the consumer moves to its own hart in step 11, **the same scenario should pass with a much tighter threshold** — that's the speedup we'll measure. Or it doesn't, because the lock contends, and that's also the post.

## the cooperative cost (the part to remember for post 2)

- under cooperative single-hart, lock_wait is genuinely zero (or one tick — the cost of measuring it). Nothing is contending. The chokepoint is doing nothing.
- this is the baseline the v0.5 prefactor was set up to demonstrate. **The chokepoint's value is invisible until contention exists.** Post 2 is "I added a second hart and the chokepoint lit up." Post 3 is "I replaced the chokepoint and watched it go dark again."
- we couldn't write that arc without first writing this baseline. So this post's job is mostly to set up the trilogy. The headline graphs are the *next* two posts'.

## footguns worth flagging

- *Mutex<Option<VecDeque>>* not *Mutex<VecDeque>*: `VecDeque::new()` isn't const, so `Mutex::new(VecDeque::new())` doesn't work at static-init time. The `Option<>` + `get_or_insert_with` pattern is how every similar static in this kernel handles non-const constructors (see `heap_smoke::TABLE`, the lazy `INTERN_TABLE` shape).
- *static array of atomics*: `[const { AtomicU64::new(0) }; BUCKETS]` — Rust 2024 inline const expressions in array repeat. Worth knowing if you've been writing macros for this.
- *batch off-lock, work off-lock, lock only the queue*: the producer generates its batch *before* acquiring the lock and the consumer bins *after* releasing it. The Mutex only holds the queue's push/pop, not the LCG iteration or the bin computation. Otherwise the lock-wait histogram lies — it would include time spent doing work that isn't shared.
- *spans don't survive yields here, deliberately*: both producer and consumer open-then-close their span inside a block, then yield. The same discipline `task_b` uses. We don't need the cross-yield span machinery for this workload (per-batch is a clean unit) and matching `task_b`'s shape keeps the cursor balanced.

## what's next

- step 2 is dashboards + this post (you're reading it).
- step 3 is the wire-format break for SMP: `HartRegister`, `hart_id` on `SpanStart` and `ContextSwitch`, protocol version bump. Done now, before any external consumer of the format exists.
- steps 4–10 are the actual SMP bring-up: `tp` register convention, per-CPU lift, IPI primitive, SBI HSM hart_start, TLB shootdown, per-hart runqueues.
- step 11 is **the second post**: take this workload, move the consumer to hart 1, watch the lock-wait graph come alive (or not).
- step 12 is **the third post**: swap `Mutex<VecDeque>` for `heapless::spsc::Queue` and watch the lock-wait graph fall off a cliff.

The chokepoint sets a single point of comparison for all three. That's the whole bet.

---

*[TBD: screenshots — Grafana dashboard with throughput, queue depth at this baseline; Tempo trace view showing `workload.produce` / `workload.consume` spans alternating with `task_a.tick` / `task_b.tick`]*
