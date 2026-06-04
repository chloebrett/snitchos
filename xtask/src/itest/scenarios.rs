//! One function per scenario. Each returns `Ok(())` on pass, or a
//! human-readable `String` describing what didn't match.

use std::time::Duration;

use protocol::stream::OwnedFrame;


use super::harness::Harness;
use super::matchers::{is_dropped, is_hello, is_metric_named, is_span_start_named, is_string_register_named};

const SEC: Duration = Duration::from_secs(1);

/// Frame allocator is initialized and exercised. Each heartbeat does
/// an `alloc_zeroed` + `free`, so the counters tick up over time. The
/// scenario waits for a `snitchos.frames.allocated_total` metric with
/// value ≥ 1, which proves: init ran, the linear map resolves (the
/// zeroing wrote 4 KiB via `pa_to_kernel_va`), and at least one
/// heartbeat completed.
pub fn frame_allocator_metrics() -> Result<(), String> {
    let mut h = Harness::spawn("frames")?;

    let frame = h
        .wait_for(SEC * 10, is_metric_named("snitchos.frames.allocated_total"))
        .ok_or("no snitchos.frames.allocated_total metric within 10s")?;
    let value = match frame {
        OwnedFrame::Metric { value, .. } => value,
        _ => return Err("matched non-metric (impossible)".to_string()),
    };
    if value < 1 {
        return Err(format!(
            "frames.allocated_total = {value}, expected ≥ 1 (linear map fault or smoke alloc never ran?)"
        ));
    }
    Ok(())
}

/// Kernel heap is initialized and exercised. Each heartbeat does a
/// `Vec::with_capacity(256)` + push + drop, so the heap counters tick
/// up over time. We assert:
///
///   1. `snitchos.heap.alloc_total` rises above 0 — `#[global_allocator]`
///      is wired, `heap::init` ran, the linear-map VA is writable.
///   2. `snitchos.heap.bytes_used` is observed — the gauge emits even
///      if the smoke leaves it near 0 after drop.
///   3. At least one heartbeat survives after — the heap doesn't
///      break the boot/loop path.
pub fn kernel_heap_metrics() -> Result<(), String> {
    let mut h = Harness::spawn("heap")?;

    let frame = h
        .wait_for(SEC * 10, is_metric_named("snitchos.heap.alloc_total"))
        .ok_or("no snitchos.heap.alloc_total metric within 10s — heap not initialised or not emitting?")?;
    let value = match frame {
        OwnedFrame::Metric { value, .. } => value,
        _ => return Err("matched non-metric (impossible)".to_string()),
    };
    if value < 1 {
        return Err(format!(
            "heap.alloc_total = {value}, expected ≥ 1 (heap init ran but smoke didn't alloc?)"
        ));
    }

    h.wait_for(SEC * 5, is_metric_named("snitchos.heap.bytes_used"))
        .ok_or("no snitchos.heap.bytes_used metric within 5s")?;

    h.wait_for(SEC * 5, is_span_start_named("kernel.heartbeat"))
        .ok_or("no heartbeat after heap metric — heap broke the loop?")?;

    Ok(())
}

/// Kernel heap grows under pressure, then exhausts cleanly. The
/// `heap-oom`-feature kernel leaks 4096 × 4 KiB blocks per heartbeat
/// (16 MiB/tick) via `Vec::try_reserve_exact` + `mem::forget`. P2's
/// watermark grow adds 1 MiB/tick when free drops below 25%, so the
/// heap visibly expands from 4 MiB toward its frame-supply ceiling
/// (~120 MiB usable) before OOM hits in ~8 heartbeats. We assert:
///
///   1. `snitchos.heap.grow_total` rises above 0 — P2's grow path
///      actually engaged, not just absorbed inside the original
///      4 MiB.
///   2. `snitchos.heap.alloc_failed_total` rises above 0 — eventual
///      OOM is still cleanly handled (null return, not panic).
///   3. Two more heartbeats arrive after — kernel survives OOM.
/// Context-switch asm round-trips correctly. After `heap::init`,
/// `kmain` calls `sched::smoke()` which builds a hand-rigged
/// `TaskContext` pointing at a marker function, switches into it,
/// and switches back. The marker bumps `SMOKE_MARKER_HITS` once.
/// The heartbeat emits the counter; this scenario asserts it
/// observed exactly 1 within budget. The asm could be wrong in
/// subtler ways than "crashes the kernel" — this scenario catches
/// e.g. corrupting callee-saved registers (would cause weird
/// failures elsewhere) or never actually entering the marker.
pub fn sched_context_switch_smoke() -> Result<(), String> {
    let mut h = Harness::spawn("schedsmoke")?;

    let frame = h
        .wait_for(SEC * 10, |f, strings| match f {
            OwnedFrame::Metric { name_id, value, .. } => {
                strings.get(name_id).map(String::as_str)
                    == Some("snitchos.sched.smoke_marker_hits")
                    && *value >= 1
            }
            _ => false,
        })
        .ok_or(
            "no sched.smoke_marker_hits >= 1 within 10s — asm switched into marker but never came back, or marker never ran, or counter not emitted",
        )?;
    let value = match frame {
        OwnedFrame::Metric { value, .. } => value,
        _ => return Err("matched non-metric (impossible)".to_string()),
    };
    if value != 1 {
        return Err(format!(
            "sched.smoke_marker_hits = {value}, expected exactly 1 (smoke runs once at init)"
        ));
    }
    Ok(())
}

pub fn heap_oom() -> Result<(), String> {
    let mut h = Harness::spawn_with_features("heap-oom", &["heap-oom"])?;

    h.wait_for(SEC * 10, |f, strings| match f {
        OwnedFrame::Metric { name_id, value, .. } => {
            strings.get(name_id).map(String::as_str)
                == Some("snitchos.heap.grow_total")
                && *value > 0
        }
        _ => false,
    })
    .ok_or(
        "no heap.grow_total > 0 within 10s — watermark grow never triggered, leak too slow, or extend() broken",
    )?;

    h.wait_for(SEC * 15, |f, strings| match f {
        OwnedFrame::Metric { name_id, value, .. } => {
            strings.get(name_id).map(String::as_str)
                == Some("snitchos.heap.alloc_failed_total")
                && *value > 0
        }
        _ => false,
    })
    .ok_or(
        "no heap.alloc_failed_total > 0 within 15s — heap grew but never OOM'd; leak too slow, or grow outpacing leak",
    )?;

    h.wait_for(SEC * 5, is_span_start_named("kernel.heartbeat"))
        .ok_or("no heartbeat within 5s after first heap alloc failure — kernel hung?")?;
    h.wait_for(SEC * 5, is_span_start_named("kernel.heartbeat"))
        .ok_or("no second heartbeat post-OOM — kernel hung after one more tick?")?;

    Ok(())
}

/// Frame allocator exhausts the pool cleanly and the kernel survives.
/// The `oom-leak`-feature kernel leaks 8192 frames per heartbeat
/// (32 MiB), so the ~32K-frame pool runs out in ~4 heartbeats on the
/// default QEMU `virt` config. We assert:
///
///   1. `snitchos.frames.alloc_failed_total` eventually rises above 0
///      — the allocator handled OOM by returning `None`, not by
///      crashing.
///   2. At least two more heartbeats arrive after the first failure
///      — the kernel didn't lock up; metrics keep flowing.
pub fn frame_allocator_oom() -> Result<(), String> {
    // Build the kernel with the `oom-leak` feature so the heartbeat
    // smoke leaks 8192 frames/tick instead of doing alloc+free.
    let mut h = Harness::spawn_with_features("oom", &["oom-leak"])?;

    // (1) Wait up to 15s for the first non-zero alloc_failed_total.
    // ~4 heartbeats × ~1s each = ~4s; 15s gives generous slack.
    h.wait_for(SEC * 15, |f, strings| match f {
        OwnedFrame::Metric { name_id, value, .. } => {
            strings.get(name_id).map(String::as_str)
                == Some("snitchos.frames.alloc_failed_total")
                && *value > 0
        }
        _ => false,
    })
    .ok_or(
        "no alloc_failed_total > 0 within 15s — leak rate too low, allocator broken, or kernel died",
    )?;

    // (2) Two more heartbeat SpanStarts post-OOM. Proves the kernel
    // didn't crash trying to alloc after exhaustion.
    h.wait_for(SEC * 5, is_span_start_named("kernel.heartbeat"))
        .ok_or("no heartbeat within 5s after first alloc failure — kernel hung?")?;
    h.wait_for(SEC * 5, is_span_start_named("kernel.heartbeat"))
        .ok_or("no second heartbeat after first alloc failure — kernel hung after one more tick?")?;

    Ok(())
}

/// Explicit assertion that the kernel runs at higher-half PC. After
/// `mmu::enable` + trampoline, the kernel reads its current PC via
/// `auipc` and only emits the `kernel.runs_at_higher_half` span if PC
/// is in the higher-half range. If a future change silently leaves PC
/// at identity (broken trampoline), the span never appears and this
/// scenario times out.
pub fn kernel_runs_at_higher_half() -> Result<(), String> {
    let mut h = Harness::spawn("higherhalf")?;
    h.wait_for(SEC * 5, is_span_start_named("kernel.runs_at_higher_half"))
        .ok_or("no kernel.runs_at_higher_half SpanStart — PC isn't actually at higher-half post-trampoline")?;
    Ok(())
}

/// Boot sequence reaches the heartbeat loop: Hello → kernel.boot
/// SpanStart → Dropped(0) (proves pre-init flush ran cleanly) →
/// first kernel.heartbeat SpanStart (proves the timer IRQ is firing).
pub fn boot_reaches_heartbeat() -> Result<(), String> {
    let mut h = Harness::spawn("boot")?;

    h.wait_for(SEC * 3, is_hello())
        .ok_or("no Hello frame within 3s")?;
    h.wait_for(SEC * 3, is_span_start_named("kernel.boot"))
        .ok_or("no kernel.boot SpanStart within 3s")?;
    h.wait_for(SEC * 5, is_dropped(0))
        .ok_or("no Dropped(0) checkpoint after flush_pre_init within 5s")?;
    h.wait_for(SEC * 10, is_span_start_named("kernel.heartbeat"))
        .ok_or("no kernel.heartbeat SpanStart within 10s — timer IRQ not firing?")?;

    Ok(())
}

/// Two consecutive heartbeat SpanStarts arrive with monotonic timestamps
/// and a sane tick interval. Captures `Hello` first to get the timebase,
/// then converts the tick delta to nanoseconds and asserts it falls
/// between 10 ms and 10 s — loose enough to survive QEMU stalls but
/// tight enough to catch a runaway or frozen timer.
pub fn heartbeat_cadence() -> Result<(), String> {
    let mut h = Harness::spawn("cadence")?;

    h.wait_for(SEC * 5, is_hello())
        .ok_or("no Hello frame within 5s")?;
    let timebase_hz = h
        .timebase_hz()
        .ok_or("Hello arrived but timebase_hz is missing")?;

    let first = h
        .wait_for(SEC * 15, is_span_start_named("kernel.heartbeat"))
        .ok_or("no first heartbeat within 15s")?;
    let second = h
        .wait_for(SEC * 5, is_span_start_named("kernel.heartbeat"))
        .ok_or("no second heartbeat within 5s of the first")?;

    let (t1, t2) = match (&first, &second) {
        (OwnedFrame::SpanStart { t: a, .. }, OwnedFrame::SpanStart { t: b, .. }) => (*a, *b),
        _ => return Err("matched frame was not a SpanStart (impossible)".to_string()),
    };
    if t2 <= t1 {
        return Err(format!("timestamps not monotonic: first={t1}, second={t2}"));
    }

    let delta_ns = (t2 - t1) as u128 * 1_000_000_000 / timebase_hz as u128;
    const MIN_NS: u128 = 10_000_000;        // 10 ms
    const MAX_NS: u128 = 10_000_000_000;    // 10 s
    if delta_ns < MIN_NS || delta_ns > MAX_NS {
        return Err(format!(
            "heartbeat interval {delta_ns} ns is outside [{MIN_NS}, {MAX_NS}] ns \
             (timebase={timebase_hz} Hz, delta={} ticks)",
            t2 - t1,
        ));
    }

    Ok(())
}

/// Pre-init buffer preserves frame order across the flush. Two
/// invariants:
///
///   1. The first `StringRegister` on the wire is for "kernel.boot"
///      — it was registered before virtio_console::init succeeded,
///      so it lived in the pre-init buffer.
///   2. Every span's `name_id` was registered earlier in the stream.
///      If the buffer dequeued out of order we'd see SpanStarts
///      referencing unknown ids.
pub fn pre_init_order() -> Result<(), String> {
    let mut h = Harness::spawn("preinit")?;

    // (1) First StringRegister we see should name kernel.boot.
    let first = h
        .wait_for(SEC * 5, is_string_register_named("kernel.boot"))
        .ok_or("no kernel.boot StringRegister within 5s — pre-init buffer drained out of order?")?;
    let OwnedFrame::StringRegister { id: _, value } = first else {
        return Err("matched non-StringRegister (impossible)".to_string());
    };
    if value != "kernel.boot" {
        return Err(format!("first StringRegister was '{value}', expected 'kernel.boot'"));
    }

    // (2) Drain through the first heartbeat. wait_for absorbs
    // StringRegister frames into the harness's string table as it
    // goes; if any SpanStart appeared whose name_id wasn't yet
    // registered, the matcher for kernel.heartbeat would never fire
    // for the WRONG reason (it'd still resolve once the register
    // arrived). So instead we check explicitly: for every SpanStart
    // we walk past, name_of(name_id) must be Some.
    let deadline = std::time::Instant::now() + SEC * 10;
    loop {
        let remaining = deadline
            .checked_duration_since(std::time::Instant::now())
            .ok_or("did not reach first heartbeat within 10s")?;
        let frame = h.wait_for(remaining, |_, _| true)
            .ok_or("stream closed before reaching first heartbeat")?;
        match frame {
            OwnedFrame::SpanStart { name_id, .. } => {
                if h.name_of(name_id).is_none() {
                    return Err(format!(
                        "SpanStart references unregistered name_id {:?} — buffer flush is out of order",
                        name_id
                    ));
                }
                if h.name_of(name_id) == Some("kernel.heartbeat") {
                    return Ok(());
                }
            }
            _ => continue,
        }
    }
}
