//! B5 investigation: do repeated Stitch state transitions accumulate memory
//! (Rc cycle → unbounded growth) or are they bounded churn (freed promptly)?
//!
//! ## Finding (2026-07-08)
//!
//! **There is an Rc cycle** — each `eval_program` call leaks its env (~60 KB
//! on the test program including the prelude). Growth is linear with run count:
//!
//! - Simple fold (50 runs):   ~59 KB after run 1 → ~3 MB after 50 runs  (~50×)
//! - Closure-heavy (50 runs): ~62 KB after run 1 → ~3.1 MB after 50 runs (~50×)
//!
//! The cycle: `EnvInner` → globals `BTreeMap` → `Closure { env: Env }` →
//! same `EnvInner`. When the env local in `eval_program_with_telemetry` is
//! dropped, `Rc` count stays > 0 because every closure in globals holds a
//! clone of the env — so the entire env+globals graph is permanently retained.
//!
//! ## Implications for the stim-vs-VM decision
//!
//! - A stim that runs as **one long Stitch program** (a loop inside `main`)
//!   is **fine** — the env is built once and lives for the run.
//! - A stim that calls `eval_program` per step **leaks linearly** — avoid.
//! - The **REPL** is fine in normal use: env is rebuilt only when defs change,
//!   not on every expression. The rebuild leaks but it's amortized.
//!
//! ## Fix (deferred)
//!
//! Break the cycle by storing globals behind a `Weak<EnvInner>` in closures,
//! or by separating the globals map from the `Env` `Rc` chain so closures can
//! share it without a back-reference to their own home env. This is a Phase-C
//! concern (the IR reshape changes `Value::Closure` to hold a code-ref + upvalues
//! rather than a captured `Env`) — the cycle goes away structurally when closures
//! stop capturing the full env.
//!
//! Run manually: `cargo test -p stitch --test memory_churn -- --nocapture --ignored`

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

static LIVE: AtomicUsize = AtomicUsize::new(0);

struct Tracker;

unsafe impl GlobalAlloc for Tracker {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            LIVE.fetch_add(layout.size(), Relaxed);
        }
        ptr
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        LIVE.fetch_sub(layout.size(), Relaxed);
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let new_ptr = unsafe { System.realloc(ptr, layout, new_size) };
        if !new_ptr.is_null() {
            LIVE.fetch_sub(layout.size(), Relaxed);
            LIVE.fetch_add(new_size, Relaxed);
        }
        new_ptr
    }
}

#[global_allocator]
static ALLOC: Tracker = Tracker;

use stitch::ast::Item;
use stitch::interp::eval_program;
use stitch::lower::lower_program;
use stitch::parser::parse_program;

fn compile(src: &str) -> Vec<Item> {
    let mut items = parse_program(src).expect("parse");
    lower_program(&mut items);
    items
}

fn snapshot() -> usize {
    LIVE.load(Relaxed)
}

/// Run `f` N times; return (retained_after_run_1, retained_after_run_N) above
/// the pre-run baseline (after one warm-up to settle lazy init).
fn measure(f: impl Fn(), n: usize) -> (usize, usize) {
    f(); // warm-up
    let before = snapshot();
    f();
    let after_1 = snapshot();
    for _ in 1..n {
        f();
    }
    let after_n = snapshot();
    (after_1.saturating_sub(before), after_n.saturating_sub(before))
}

const RUNS: usize = 50;

/// Each `eval_program` call must not accumulate live bytes — env must be freed
/// when the call returns (no Rc cycle). Growth above 2× the first run's cost
/// over 50 runs indicates a retained cycle.
#[test]
fn eval_program_does_not_accumulate_memory() {
    let simple = compile("step(acc) = acc + 1  main() = fold(1..100, 0, (acc, _) -> step(acc))");
    let (s1, sn) = measure(|| { eval_program(&simple).expect("run"); }, RUNS);
    eprintln!("simple fold:       after run 1 = {s1:>8} B, after {RUNS} runs = {sn:>10} B  (ratio {:.1}×)", sn as f64 / s1.max(1) as f64);

    let heavy = compile("wrap(x) = () -> x  main() = fold(1..100, wrap(0), (acc, _) -> wrap(acc() + 1))()");
    let (h1, hn) = measure(|| { eval_program(&heavy).expect("run"); }, RUNS);
    eprintln!("closure-heavy fold: after run 1 = {h1:>8} B, after {RUNS} runs = {hn:>10} B  (ratio {:.1}×)", hn as f64 / h1.max(1) as f64);

    let limit = s1.max(1024) * 2;
    assert!(sn <= limit,
        "simple fold: live bytes grew from {s1} (run 1) to {sn} (after {RUNS} runs) — Rc cycle not fixed (limit: {limit})");
    let limit = h1.max(1024) * 2;
    assert!(hn <= limit,
        "closure-heavy fold: live bytes grew from {h1} (run 1) to {hn} (after {RUNS} runs) — Rc cycle not fixed (limit: {limit})");
}
