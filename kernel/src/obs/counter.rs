//! [`DeferredCounter`] — a metric counter bumped from a hot path and drained
//! once per heartbeat, plus the registry the heartbeat iterates.

use core::sync::atomic::{AtomicU64, Ordering};

use protocol::StringId;

use crate::sync::Once;

/// A monotonic metric counter bumped (cheaply, `Relaxed`) from any hot path —
/// IRQ handler, allocator, trap — and drained once per tick by the heartbeat.
///
/// It bundles the atomic with its wire name + interned [`StringId`], so a
/// subsystem declares the metric name *where* it declares the counter and the
/// heartbeat drains the whole [registry](register_all) in one loop. Emission
/// stays deferred by construction: the bump site only touches the atomic; the
/// heartbeat (main thread, no allocator/virtio mutex held) does the interning +
/// frame emit. Emitting from the bump site would re-enter the intern /
/// `virtio_console` mutexes — the deadlock the deferred pattern exists to avoid.
pub struct DeferredCounter {
    value: AtomicU64,
    name: &'static str,
    id: Once<StringId>,
}

impl DeferredCounter {
    /// Declare a counter with its wire metric name
    /// (e.g. `"snitchos.frames.allocated_total"`).
    #[must_use]
    pub const fn new(name: &'static str) -> Self {
        Self { value: AtomicU64::new(0), name, id: Once::new() }
    }

    /// Bump by one. Lock-free; safe from any context.
    pub fn inc(&self) {
        self.value.fetch_add(1, Ordering::Relaxed);
    }

    /// Bump by `n`.
    pub fn add(&self, n: u64) {
        self.value.fetch_add(n, Ordering::Relaxed);
    }

    /// The current value.
    #[must_use]
    pub fn load(&self) -> u64 {
        self.value.load(Ordering::Relaxed)
    }

    /// Intern the metric name once (cached). Called at boot for every registered
    /// counter so the `StringRegister` frames land at boot — before the first
    /// drain — exactly as the old `Metrics::register` did.
    pub fn register(&self) {
        self.id.call_once(|| crate::tracing::register_counter(self.name));
    }

    /// Emit the current value to its metric. No-op until [`register`](Self::register)
    /// has interned the name.
    pub fn drain(&self) {
        if let Some(id) = self.id.get() {
            crate::tracing::emit_metric(*id, self.load() as i64);
        }
    }
}

/// Every [`DeferredCounter`] the heartbeat drains — the single place they're
/// enumerated for draining; each counter itself lives in its own subsystem.
static COUNTERS: &[&DeferredCounter] = &[
    // frame allocator
    &crate::frame::ALLOC_COUNT,
    &crate::frame::FREE_COUNT,
    &crate::frame::ALLOC_FAIL_COUNT,
    // kernel heap
    &crate::heap::ALLOC_COUNT,
    &crate::heap::DEALLOC_COUNT,
    &crate::heap::ALLOC_FAIL_COUNT,
    &crate::heap::GROW_COUNT,
    &crate::heap::GROW_FAIL_COUNT,
    // scheduler
    &crate::sched::CONTEXT_SWITCHES,
    &crate::sched::PREEMPTIONS,
    &crate::sched::SMOKE_MARKER_HITS,
    &crate::sched::EXIT_SMOKE_HITS,
    &crate::sched::WAKE_RESUMED,
    // IPC
    &crate::ipc::MESSAGES_TOTAL,
    &crate::ipc::BLOCKS_TOTAL,
    &crate::ipc::CALLS_TOTAL,
    &crate::ipc::REPLIES_TOTAL,
    // demo tasks
    &crate::demo_tasks::TASK_A_LOOPS,
    &crate::demo_tasks::TASK_B_LOOPS,
    // workload (samples_consumed stays bespoke — Acquire-ordered oracle)
    &crate::workload::SAMPLES_PRODUCED,
    &crate::workload::LOCK_WAIT_TICKS_TOTAL,
    // SMP / IPI / MMU
    &crate::ipi::RECEIVED_TOTAL,
    &crate::ipi::SHOOTDOWNS_RECEIVED_TOTAL,
    &crate::mmu::SHOOTDOWNS_SENT_TOTAL,
    &crate::secondary::SECONDARY_WFI_COUNT,
    &crate::secondary::PROBE_TICKS,
];

/// Intern every registered counter's name. Call once at boot, before the
/// heartbeat takes over, so registration (and its `StringRegister` frames) is a
/// boot-time event.
pub fn register_all() {
    for c in COUNTERS {
        c.register();
    }
}

/// Emit every registered counter's current value. Called once per heartbeat tick.
pub fn drain_all() {
    for c in COUNTERS {
        c.drain();
    }
}
