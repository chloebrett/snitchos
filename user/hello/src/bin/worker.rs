//! Demo worker (`workload=workers`): a cooperative userspace task that loops
//! { open a `worker.tick` span, bump a progress counter, `yield` }. The
//! userspace successor to the kernel-mode `task_a`/`task_b`.
//!
//! `rust_main` never returns — it's a server loop; the runtime's post-`main`
//! `exit()` is simply never reached. The span guard opens at the top of each
//! iteration, stays open across the `yield` (span-survives-yield), and closes
//! at the end of the loop body. The span name is interned once (repeats are
//! free under the per-process quota). Step 5 gives each worker a distinct
//! `format!`-ed name + counter; for one worker the bootstrap sink is enough.

#![no_std]
#![no_main]

use snitchos_user::{Startup, yield_now};

#[unsafe(no_mangle)]
pub extern "C" fn rust_main(startup: Startup) {
    let tracer = startup.tracer();
    let sink = startup.telemetry();
    let mut progress: i64 = 0;
    loop {
        let _span = tracer.span("worker.tick");
        progress += 1;
        let _ = sink.emit(progress);
        yield_now();
    }
}
