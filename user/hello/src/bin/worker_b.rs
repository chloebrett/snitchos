//! Demo worker B (`workload=workers`): a cooperative userspace task that loops
//! { open a `worker_b.tick` span, bump a progress counter, `yield` }. The twin
//! of `worker_a` — a second, fully independent process (its own page table and
//! capabilities) sharing the same hart. Distinct span name so the two workers'
//! activity is individually attributable on the wire; the cooperative `yield`
//! is what lets them take turns without either starving.

#![no_std]
#![no_main]

use snitchos_std::thread;
use snitchos_user::{entry, telemetry, tracer};

#[entry]
fn main() {
    let tracer = tracer();
    let sink = telemetry();
    let mut progress: i64 = 0;
    loop {
        let _span = tracer.span("worker_b.tick");
        progress += 1;
        let _ = sink.emit(progress);
        // The cooperative yield, via the std-shaped facade (→ `Yield` syscall).
        thread::yield_now();
    }
}
