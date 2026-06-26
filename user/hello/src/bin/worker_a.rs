//! Demo worker A (`workload=workers`): a cooperative userspace task that loops
//! { open a `worker_a.tick` span, bump a progress counter, `yield` }. One of two
//! independent worker processes (the other is `worker_b`) that share a single
//! hart cooperatively — the userspace successor to the kernel-mode
//! `task_a`/`task_b`.
//!
//! `main` never returns — it's a server loop; the runtime's post-`main`
//! `exit()` is simply never reached. The span guard opens at the top of each
//! iteration, stays open across the `yield` (span-survives-yield), and closes
//! at the end of the loop body. The span name is interned once (repeats are
//! free under the per-process quota), and is distinct from `worker_b`'s so the
//! two workers' spans are individually attributable on the wire.

#![no_std]
#![no_main]

use snitchos_std::thread;
use snitchos_user::{entry, register_counter, tracer};

#[entry]
fn main() {
    let tracer = tracer();
    let progress_metric = register_counter("snitchos.worker_a.marker");
    let mut progress: i64 = 0;
    loop {
        let _span = tracer.span("worker_a.tick");
        progress += 1;
        progress_metric.emit(progress);
        // The cooperative yield, via the std-shaped facade (→ `Yield` syscall).
        thread::yield_now();
    }
}
