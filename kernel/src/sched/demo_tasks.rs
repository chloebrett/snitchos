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
//! `kmain` spawns these on the default-demo path; a non-default
//! `workload=` selection runs a different set (the cross-hart workload
//! or a storm) instead.

use core::arch::asm;

use crate::counter::DeferredCounter;
use crate::sched;
use crate::span;

pub static TASK_A_LOOPS: DeferredCounter = DeferredCounter::new("snitchos.task_a.loops");
pub static TASK_B_LOOPS: DeferredCounter = DeferredCounter::new("snitchos.task_b.loops");

/// Idle thread. The "what runs when nothing else wants the CPU" task. Only
/// `wfi` when the runqueue is genuinely empty — if a task is `Ready` (idle was
/// picked while real work waits), sleeping would strand it until the next timer
/// IRQ; instead fall straight through to `yield_now` and hand it the CPU.
pub extern "C" fn idle_entry() -> ! {
    loop {
        if !sched::has_ready_tasks() {
            unsafe { asm!("wfi") };
        }
        sched::yield_now();
    }
}

/// Burn an appreciable amount of CPU so the per-task `cpu_time_ticks`
/// rate is visible against idle's wfi-dominated time. ~15M LCG iters
/// is ~50ms of wallclock on QEMU virt; `task_b` doubles it.
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
            TASK_A_LOOPS.inc();
        }
        sched::yield_now();
    }
}

pub extern "C" fn task_b_entry() -> ! {
    loop {
        {
            span!("task_b.tick");
            burn_lcg(900_000);
            TASK_B_LOOPS.inc();
        }
        sched::yield_now();
    }
}
