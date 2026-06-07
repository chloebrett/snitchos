//! Demo workload tasks used by the default kernel boot. Two ticking
//! tasks (`task_a`, `task_b`) plus an `idle` thread that owns the
//! kernel's `wfi`. Each ticking task opens a per-iteration
//! `task_x.tick` span, bumps its counter, and yields. With main and
//! idle in the mix the scheduler round-robins through all four;
//! both tasks' `tick` spans interleave on the wire, each correctly
//! tagged with its own `task_id`.
//!
//! `task_a`'s span is opened-and-yields-mid-span to exercise the
//! "span survives a context switch" path. Per-task `SpanCursor`
//! swapping on context switch is what makes this safe; until v0.5
//! step 7, the discipline was "balance the cursor before yielding."
//!
//! All three entries are gated dead-code under the deflake feature
//! flags (kmain doesn't spawn them under any of those features).

use core::arch::asm;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::sched;
use crate::span;

/// `Relaxed`: pure tallies. See `kernel::percpu` for the kernel-wide
/// ordering discipline.
pub static TASK_A_LOOPS: AtomicU64 = AtomicU64::new(0);
pub static TASK_B_LOOPS: AtomicU64 = AtomicU64::new(0);

/// Idle thread. The "what runs when nothing else wants the CPU"
/// task. `wfi` sleeps until any interrupt arrives (timer being the
/// only one v0.5 cares about); the subsequent `yield_now` hands
/// control to whoever is now ready.
pub extern "C" fn idle_entry() -> ! {
    loop {
        unsafe { asm!("wfi") };
        sched::yield_now();
    }
}

/// Burn an appreciable amount of CPU so the per-task `cpu_time_ticks`
/// rate is visible against idle's wfi-dominated time. ~15M LCG iters
/// is ~50ms of wallclock on QEMU virt; task_b doubles it.
///
/// `black_box(x)` keeps the loop body from being optimised out — the
/// LCG state has to look observable to the compiler.
fn burn_lcg(iterations: u32) {
    let mut x: u64 = 1;
    for _ in 0..iterations {
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    }
    let _ = core::hint::black_box(x);
}

#[cfg_attr(
    any(
        feature = "deflake-spawn-storm",
        feature = "deflake-ipi-pong",
        feature = "deflake-shootdown-storm"
    ),
    allow(dead_code)
)]
pub extern "C" fn task_a_entry() -> ! {
    loop {
        {
            span!("task_a.tick");
            // Split the work around a yield to exercise the
            // "span survives a context switch" path. Per-task
            // SpanCursor means task_b's spans opened in between
            // don't get parented to this still-open span.
            burn_lcg(150_000);
            sched::yield_now();
            burn_lcg(150_000);
            TASK_A_LOOPS.fetch_add(1, Ordering::Relaxed);
        }
        sched::yield_now();
    }
}

#[cfg_attr(
    any(
        feature = "deflake-spawn-storm",
        feature = "deflake-ipi-pong",
        feature = "deflake-shootdown-storm"
    ),
    allow(dead_code)
)]
pub extern "C" fn task_b_entry() -> ! {
    loop {
        {
            span!("task_b.tick");
            burn_lcg(900_000);
            TASK_B_LOOPS.fetch_add(1, Ordering::Relaxed);
        }
        sched::yield_now();
    }
}
