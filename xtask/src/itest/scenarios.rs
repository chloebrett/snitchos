//! One function per scenario. Each returns `Ok(())` on pass, or a
//! human-readable `String` describing what didn't match.

use std::time::Duration;

use protocol::stream::OwnedFrame;


use super::harness::Harness;
use super::matchers::{is_dropped, is_hello, is_metric_named, is_span_start_named, is_string_register_named, is_thread_register_named};

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
        .wait_for(SEC * 30, is_metric_named("snitchos.frames.allocated_total"))
        .ok_or("no snitchos.frames.allocated_total metric within 30s")?;
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
        .wait_for(SEC * 30, is_metric_named("snitchos.heap.alloc_total"))
        .ok_or("no snitchos.heap.alloc_total metric within 30s — heap not initialised or not emitting?")?;
    let value = match frame {
        OwnedFrame::Metric { value, .. } => value,
        _ => return Err("matched non-metric (impossible)".to_string()),
    };
    if value < 1 {
        return Err(format!(
            "heap.alloc_total = {value}, expected ≥ 1 (heap init ran but smoke didn't alloc?)"
        ));
    }

    h.wait_for(SEC * 20, is_metric_named("snitchos.heap.bytes_used"))
        .ok_or("no snitchos.heap.bytes_used metric within 20s")?;

    h.wait_for(SEC * 20, is_span_start_named("kernel.heartbeat"))
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
///
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
        .wait_for(SEC * 30, |f, strings| match f {
            OwnedFrame::Metric { name_id, value, .. } => {
                strings.get(name_id).map(String::as_str)
                    == Some("snitchos.sched.smoke_marker_hits")
                    && *value >= 1
            }
            _ => false,
        })
        .ok_or(
            "no sched.smoke_marker_hits >= 1 within 30s — asm switched into marker but never came back, or marker never ran, or counter not emitted",
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

/// `kmain` registers task 0 as "main" via `register_bare_task` and
/// spawns "idle", "task_a", "task_b" via `spawn(name, entry)`. Each
/// call emits a `ThreadRegister` frame. This scenario asserts all
/// four appear within budget, proving `spawn` builds + queues each
/// task and the wire carries names through to the collector.
pub fn sched_spawn_registers_thread() -> Result<(), String> {
    let mut h = Harness::spawn("schedspawn")?;

    h.wait_for(SEC * 20, is_thread_register_named("main"))
        .ok_or("no ThreadRegister for 'main' within 20s")?;
    h.wait_for(SEC * 20, is_thread_register_named("idle"))
        .ok_or("no ThreadRegister for 'idle' within 20s")?;
    h.wait_for(SEC * 20, is_thread_register_named("task_a"))
        .ok_or("no ThreadRegister for 'task_a' within 20s")?;
    h.wait_for(SEC * 20, is_thread_register_named("task_b"))
        .ok_or("no ThreadRegister for 'task_b' within 20s")?;

    Ok(())
}

/// Cooperative round-robin works: main, idle, task_a, task_b are all
/// taking turns. We assert both demo tasks' loop counters rise above
/// 0 within budget, plus the scheduler's cumulative switch counter
/// climbs. That triplet rules out "yield_now does nothing" and "only
/// one task runs."
pub fn sched_yield_round_trips() -> Result<(), String> {
    let mut h = Harness::spawn("schedyield")?;

    h.wait_for(SEC * 45, |f, strings| match f {
        OwnedFrame::Metric { name_id, value, .. } => {
            strings.get(name_id).map(String::as_str)
                == Some("snitchos.task_a.loops")
                && *value > 0
        }
        _ => false,
    })
    .ok_or("no task_a.loops > 0 within 45s")?;

    h.wait_for(SEC * 45, |f, strings| match f {
        OwnedFrame::Metric { name_id, value, .. } => {
            strings.get(name_id).map(String::as_str)
                == Some("snitchos.task_b.loops")
                && *value > 0
        }
        _ => false,
    })
    .ok_or("no task_b.loops > 0 within 45s — round-robin not reaching task_b")?;

    h.wait_for(SEC * 45, |f, strings| match f {
        OwnedFrame::Metric { name_id, value, .. } => {
            strings.get(name_id).map(String::as_str)
                == Some("snitchos.sched.context_switches_total")
                && *value > 0
        }
        _ => false,
    })
    .ok_or("no sched.context_switches_total > 0 within 45s")?;

    Ok(())
}

/// A span that's open across a `yield_now` closes correctly when the
/// task is resumed. task_a opens `task_a.tick`, yields mid-span,
/// gets re-scheduled, then closes. The wire should show:
///
///   1. SpanStart for "task_a.tick" with `task_id == task_a_id`,
///      `parent == SpanId(0)` (top-level — proves per-task cursor
///      isn't being polluted by other tasks' spans).
///   2. At least one ContextSwitch leaving task_a, and one returning.
///   3. SpanEnd for the same span id as (1).
///
/// Without per-task `SpanCursor` wiring, the parent in (1) could be
/// any other task's currently-open span, and (3)'s pop would land on
/// the wrong cursor. This scenario is the structural proof that the
/// per-task wiring works.
pub fn sched_span_survives_yield() -> Result<(), String> {
    use protocol::SpanId;

    let mut h = Harness::spawn("schedspansurvive")?;

    let task_a_reg = h
        .wait_for(SEC * 20, is_thread_register_named("task_a"))
        .ok_or("no ThreadRegister for 'task_a'")?;
    let task_a_id = match task_a_reg {
        OwnedFrame::ThreadRegister { id, .. } => id,
        _ => return Err("matched non-ThreadRegister".to_string()),
    };

    let span_start = h
        .wait_for(SEC * 45, |f, strings| match f {
            OwnedFrame::SpanStart { name_id, task_id, parent, .. } => {
                strings.get(name_id).map(String::as_str) == Some("task_a.tick")
                    && *task_id == task_a_id
                    && *parent == SpanId(0)
            }
            _ => false,
        })
        .ok_or(
            "no top-level SpanStart 'task_a.tick' on task_a within 45s — wiring may have parented it to another task's span",
        )?;
    let span_id = match span_start {
        OwnedFrame::SpanStart { id, .. } => id,
        _ => return Err("matched non-SpanStart".to_string()),
    };

    h.wait_for(SEC * 30, move |f, _| match f {
        OwnedFrame::ContextSwitch { from, .. } => *from == task_a_id,
        _ => false,
    })
    .ok_or("no ContextSwitch leaving task_a within 30s after the span opened")?;

    h.wait_for(SEC * 30, move |f, _| match f {
        OwnedFrame::ContextSwitch { to, .. } => *to == task_a_id,
        _ => false,
    })
    .ok_or("no ContextSwitch returning to task_a within 30s — task_a was orphaned mid-span")?;

    h.wait_for(SEC * 30, move |f, _| match f {
        OwnedFrame::SpanEnd { id, .. } => *id == span_id,
        _ => false,
    })
    .ok_or("no SpanEnd matching the surviving span's id within 30s — close popped the wrong cursor or never ran")?;

    Ok(())
}

/// `ContextSwitch` frames arrive on the wire with sane `from` / `to`
/// values. We harvest the ThreadRegister id for each known task,
/// then wait for a ContextSwitch frame whose endpoints are both
/// recognised task ids and whose reason is `Yield` (only switch
/// flavour in cooperative v0.5). Proves the scheduler is emitting
/// the per-switch event, not just the cumulative counter.
pub fn sched_context_switches_on_wire() -> Result<(), String> {
    use std::collections::HashSet;

    let mut h = Harness::spawn("schedcs")?;

    let mut task_ids: HashSet<u32> = HashSet::new();
    for name in ["main", "idle", "task_a", "task_b"] {
        let frame = h
            .wait_for(SEC * 20, is_thread_register_named(name))
            .ok_or_else(|| std::format!("no ThreadRegister for '{name}'"))?;
        if let OwnedFrame::ThreadRegister { id, .. } = frame {
            task_ids.insert(id);
        }
    }

    h.wait_for(SEC * 30, move |f, _| match f {
        OwnedFrame::ContextSwitch { from, to, reason, .. } => {
            task_ids.contains(from)
                && task_ids.contains(to)
                && from != to
                && matches!(reason, protocol::SwitchReason::Yield)
        }
        _ => false,
    })
    .ok_or(
        "no ContextSwitch{Yield} with both endpoints being known task ids within 30s",
    )?;

    Ok(())
}

/// Each demo task emits a `task_x.tick` span per iteration. Asserts
/// that within budget we see both `task_a.tick` and `task_b.tick`
/// SpanStart frames on the wire, and each carries its own `task_id`
/// (matching the `ThreadRegister` for its name). Proves spans are
/// correctly tagged to the task that emitted them.
pub fn sched_spans_carry_task_id() -> Result<(), String> {
    let mut h = Harness::spawn("schedspans")?;

    // First the ThreadRegisters so we know the id↔name mapping.
    let task_a_reg = h
        .wait_for(SEC * 20, is_thread_register_named("task_a"))
        .ok_or("no ThreadRegister for 'task_a'")?;
    let task_b_reg = h
        .wait_for(SEC * 20, is_thread_register_named("task_b"))
        .ok_or("no ThreadRegister for 'task_b'")?;
    let task_a_id = match task_a_reg {
        OwnedFrame::ThreadRegister { id, .. } => id,
        _ => return Err("matched non-ThreadRegister".to_string()),
    };
    let task_b_id = match task_b_reg {
        OwnedFrame::ThreadRegister { id, .. } => id,
        _ => return Err("matched non-ThreadRegister".to_string()),
    };

    h.wait_for(SEC * 45, |f, strings| match f {
        OwnedFrame::SpanStart { name_id, task_id, .. } => {
            strings.get(name_id).map(String::as_str) == Some("task_a.tick")
                && *task_id == task_a_id
        }
        _ => false,
    })
    .ok_or("no SpanStart 'task_a.tick' with task_id matching task_a's ThreadRegister")?;

    h.wait_for(SEC * 45, |f, strings| match f {
        OwnedFrame::SpanStart { name_id, task_id, .. } => {
            strings.get(name_id).map(String::as_str) == Some("task_b.tick")
                && *task_id == task_b_id
        }
        _ => false,
    })
    .ok_or("no SpanStart 'task_b.tick' with task_id matching task_b's ThreadRegister")?;

    Ok(())
}

pub fn heap_oom() -> Result<(), String> {
    let mut h = Harness::spawn_with_features("heap-oom", &["heap-oom"])?;

    h.wait_for(SEC * 30, |f, strings| match f {
        OwnedFrame::Metric { name_id, value, .. } => {
            strings.get(name_id).map(String::as_str)
                == Some("snitchos.heap.grow_total")
                && *value > 0
        }
        _ => false,
    })
    .ok_or(
        "no heap.grow_total > 0 within 30s — watermark grow never triggered, leak too slow, or extend() broken",
    )?;

    h.wait_for(SEC * 45, |f, strings| match f {
        OwnedFrame::Metric { name_id, value, .. } => {
            strings.get(name_id).map(String::as_str)
                == Some("snitchos.heap.alloc_failed_total")
                && *value > 0
        }
        _ => false,
    })
    .ok_or(
        "no heap.alloc_failed_total > 0 within 45s — heap grew but never OOM'd; leak too slow, or grow outpacing leak",
    )?;

    h.wait_for(SEC * 20, is_span_start_named("kernel.heartbeat"))
        .ok_or("no heartbeat within 20s after first heap alloc failure — kernel hung?")?;
    h.wait_for(SEC * 20, is_span_start_named("kernel.heartbeat"))
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
    h.wait_for(SEC * 45, |f, strings| match f {
        OwnedFrame::Metric { name_id, value, .. } => {
            strings.get(name_id).map(String::as_str)
                == Some("snitchos.frames.alloc_failed_total")
                && *value > 0
        }
        _ => false,
    })
    .ok_or(
        "no alloc_failed_total > 0 within 45s — leak rate too low, allocator broken, or kernel died",
    )?;

    // (2) Two more heartbeat SpanStarts post-OOM. Proves the kernel
    // didn't crash trying to alloc after exhaustion.
    h.wait_for(SEC * 20, is_span_start_named("kernel.heartbeat"))
        .ok_or("no heartbeat within 20s after first alloc failure — kernel hung?")?;
    h.wait_for(SEC * 20, is_span_start_named("kernel.heartbeat"))
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
    h.wait_for(SEC * 20, is_span_start_named("kernel.runs_at_higher_half"))
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
    h.wait_for(SEC * 20, is_dropped(0))
        .ok_or("no Dropped(0) checkpoint after flush_pre_init within 20s")?;
    h.wait_for(SEC * 30, is_span_start_named("kernel.heartbeat"))
        .ok_or("no kernel.heartbeat SpanStart within 30s — timer IRQ not firing?")?;

    Ok(())
}

/// Two consecutive heartbeat SpanStarts arrive with monotonic timestamps
/// and a sane tick interval. Captures `Hello` first to get the timebase,
/// then converts the tick delta to nanoseconds and asserts it falls
/// between 10 ms and 10 s — loose enough to survive QEMU stalls but
/// tight enough to catch a runaway or frozen timer.
pub fn heartbeat_cadence() -> Result<(), String> {
    let mut h = Harness::spawn("cadence")?;

    h.wait_for(SEC * 20, is_hello())
        .ok_or("no Hello frame within 20s")?;
    let timebase_hz = h
        .timebase_hz()
        .ok_or("Hello arrived but timebase_hz is missing")?;

    let first = h
        .wait_for(SEC * 45, is_span_start_named("kernel.heartbeat"))
        .ok_or("no first heartbeat within 45s")?;
    let second = h
        .wait_for(SEC * 20, is_span_start_named("kernel.heartbeat"))
        .ok_or("no second heartbeat within 20s of the first")?;

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
    if !(MIN_NS..=MAX_NS).contains(&delta_ns) {
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
        .wait_for(SEC * 20, is_string_register_named("kernel.boot"))
        .ok_or("no kernel.boot StringRegister within 20s — pre-init buffer drained out of order?")?;
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
    let deadline = std::time::Instant::now() + SEC * 30;
    loop {
        let remaining = deadline
            .checked_duration_since(std::time::Instant::now())
            .ok_or("did not reach first heartbeat within 30s")?;
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

/// v0.6 step 10: cross-hart spawn. Boot hart calls
/// `spawn_on(1, "hart_1_probe", probe_entry)`, which puts the task on
/// hart 1's runqueue and sends `IPI_WAKEUP`. Hart 1 takes the IPI,
/// breaks `wfi`, yields, picks the probe, and the probe's loop
/// increments `PROBE_TICKS`. The scenario asserts the metric reaches
/// at least 10 within 30s — proves the whole chain works:
/// per-hart runqueue, cross-hart spawn enqueue, IPI wakeup, hart 1's
/// trap+dispatch, `yield_now` on hart 1, task execution.
pub fn smp_spawn_on_hart_1_runs() -> Result<(), String> {
    let mut h = Harness::spawn("smp-spawn")?;

    // Threshold = 3 (not 10) because hart 1's timer is 1 Hz and the
    // probe ticks once per wfi-wake-yield cycle; 10 ticks needs ~10s
    // sim, which has no margin against the 10s budget. 3 still proves
    // the chain (spawn_on → IPI → wfi-wake → yield → execute) and
    // converges in ~3s sim.
    h.wait_for(SEC * 30, |f, strings| match f {
        OwnedFrame::Metric { name_id, value, .. } => {
            strings.get(name_id).map(String::as_str)
                == Some("snitchos.smp.hart_1_probe_ticks_total")
                && *value >= 3
        }
        _ => false,
    })
    .ok_or(
        "hart_1_probe_ticks_total never reached 3 within 30s — \
         hart 1 didn't pick up the spawn_on'd task. Per-hart runqueue \
         not wired, IPI_WAKEUP not delivered, hart 1 not handling \
         software interrupts, or hart_1_main's yield_now broken.",
    )?;
    Ok(())
}

/// v0.6 step 8: secondary hart bring-up. After SBI `hart_start`,
/// hart 1 runs `_secondary_start` asm (sets sp, loads SATP,
/// trampolines to higher-half) and enters `secondary_main`, which
/// initialises per-CPU state and emits `HartRegister { id: 1 }`.
/// The scenario asserts the frame appears on the wire within 20s.
///
/// Proves: SBI HSM ECALL works, the secondary entry asm correctly
/// sets up sp + SATP + tp, hart 1 reaches higher-half + Rust, and
/// the wire-format `HartRegister` variant carries through the
/// collector.
pub fn smp_secondary_hart_boots() -> Result<(), String> {
    let mut h = Harness::spawn("smp-boot")?;

    h.wait_for(SEC * 20, |f, _| {
        matches!(f, OwnedFrame::HartRegister { id: 1, .. })
    })
    .ok_or(
        "no HartRegister{id:1} within 20s — hart 1 didn't reach \
         secondary_main, or the SATP/sp setup faulted silently, or \
         SBI hart_start returned an error",
    )?;
    Ok(())
}

/// v0.6 step 7: IPI primitive smoke. Boot hart sends itself a
/// `Wakeup` IPI after init; the software-interrupt trap handler
/// reads the pending bitflags, dispatches, and bumps
/// `snitchos.ipi.received_total`. We assert the counter reaches
/// at least 1 within 30s — proves:
///
///   1. SBI `send_ipi` ECALL works (the IPI was raised)
///   2. SSIE is enabled in `sie` (the interrupt was taken)
///   3. The trap handler routes `SupervisorSoftwareInterrupt`
///   4. `ipi_pending` Release/Acquire pair carries the bitflag
///      across the IRQ boundary
///   5. The dispatcher recognises `IPI_WAKEUP` and runs its handler
///
/// Single-hart smoke: target is `current_hartid()`. Cross-hart
/// delivery lands when secondary harts boot in step 8.
pub fn ipi_self_wakeup() -> Result<(), String> {
    let mut h = Harness::spawn("ipi-self")?;

    h.wait_for(SEC * 30, |f, strings| match f {
        OwnedFrame::Metric { name_id, value, .. } => {
            strings.get(name_id).map(String::as_str)
                == Some("snitchos.ipi.received_total")
                && *value >= 1
        }
        _ => false,
    })
    .ok_or(
        "ipi.received_total never reached 1 within 30s — \
         SBI send_ipi failed, SSIE not enabled, trap handler didn't \
         route software interrupt, or the dispatcher didn't process \
         the pending bit",
    )?;

    Ok(())
}

/// v0.6 step 1: cooperative single-hart producer/consumer histogram.
/// Producer task generates LCG samples in batches; consumer task
/// drains them under a `kernel::sync::Mutex` and bins them into a
/// `[AtomicU64; BUCKETS]` histogram. Heartbeat emits:
///
///   - `snitchos.workload.samples_consumed_total` — every sample the
///     consumer pulled from the queue
///   - `snitchos.workload.histogram_sum` — sum of all bin counts at
///     heartbeat-sample time
///
/// The invariant the consumer must uphold is: every sample it pulls
/// from the queue gets binned exactly once. Therefore
/// `histogram_sum >= samples_consumed_total` always (with equality
/// when sampled at the same instant; histogram_sum may briefly
/// trail by one batch if sampled mid-consume). If a consumer mutant
/// dropped or double-counted samples, this invariant fails.
///
/// We assert:
///   1. `samples_consumed_total >= 500` within 45s — workload is
///      actually running, both tasks are getting CPU under the
///      cooperative scheduler. The threshold trails the demo tasks'
///      heavy `burn_lcg` CPU draw; under SMP (v0.6 step 11) the
///      consumer runs on its own hart and this can be tightened.
///   2. `histogram_sum` eventually reaches at least the consumed
///      count we observed — proves the bin-on-consume path runs
///      for every sample, no drops.
pub fn workload_cooperative_baseline() -> Result<(), String> {
    let mut h = Harness::spawn("workload")?;

    // Threshold = 200 (not 500). 200 samples still requires the
    // consumer to have been scheduled multiple times — far above
    // "ran zero times" — while converging in ~3-4s sim instead of
    // 8-9, leaving comfortable margin against the 15s budget.
    let frame = h
        .wait_for(SEC * 45, |f, strings| match f {
            OwnedFrame::Metric { name_id, value, .. } => {
                strings.get(name_id).map(String::as_str)
                    == Some("snitchos.workload.samples_consumed_total")
                    && *value >= 200
            }
            _ => false,
        })
        .ok_or(
            "samples_consumed_total never reached 200 within 45s — \
             workload not running, or scheduler not giving consumer CPU?",
        )?;
    let consumed = match frame {
        OwnedFrame::Metric { value, .. } => value,
        _ => return Err("matched non-metric (impossible)".to_string()),
    };

    h.wait_for(SEC * 20, move |f, strings| match f {
        OwnedFrame::Metric { name_id, value, .. } => {
            strings.get(name_id).map(String::as_str)
                == Some("snitchos.workload.histogram_sum")
                && *value >= consumed
        }
        _ => false,
    })
    .ok_or(format!(
        "histogram_sum never reached {consumed} within 20s after \
         observing samples_consumed_total={consumed} — consumer pulled \
         samples from the queue but did not bin them (lost samples?)"
    ))?;

    Ok(())
}

/// Cross-hart spawn storm. Hart 0 calls `spawn_on(1, deflake_body)` in
/// a serialised loop: each iteration is one trial of the residual
/// memory-ordering race on hart 1's IPI pickup path. Each task bumps
/// `ACK_COUNTER` from its body; hart 0's wait-poll is MMIO-fenced via
/// a UART LSR read so its cross-hart Acquire is guaranteed-fresh
/// (decouples scenario failures from the symmetric load-side flake).
///
/// Asserts `snitchos.deflake.spawn_storm_acks` reaches `N` (200) within
/// 30 s. Under the trap-return `tag()` fix this should pass 100/100.
/// With the fix removed it should flake at ≥80% per run.
///
/// Built with `--features deflake-spawn-storm` so the default boot
/// workload is replaced by the storm; the gating also turns off the
/// per-spawn `emit_thread_register` so no incidental BQL fence closes
/// the window mid-storm. See `plans/residual-race-investigation.md`.
pub fn deflake_spawn_storm() -> Result<(), String> {
    let mut h = Harness::spawn_with_features(
        "deflake-spawn-storm",
        &["deflake-spawn-storm"],
    )?;

    h.wait_for(SEC * 30, |f, strings| match f {
        OwnedFrame::Metric { name_id, value, .. } => {
            strings.get(name_id).map(String::as_str)
                == Some("snitchos.deflake.spawn_storm_acks")
                && *value >= 200
        }
        _ => false,
    })
    .ok_or(
        "snitchos.deflake.spawn_storm_acks never reached 200 within \
         30s — hart 1 failed to pick up one of the spawn_on'd tasks, \
         likely the residual cross-hart memory-ordering race on \
         the IPI → switch path. See plans/residual-race-investigation.md.",
    )?;
    Ok(())
}

/// Tight IPI_WAKEUP storm from hart 0 to hart 1. Each iteration of the
/// inner loop is one `hart 1 in wfi → IPI → trap → swap-Acquire → sret
/// → resume` trial. At N=10 000 and ~100 µs pacing, the full storm
/// takes ~1 s wall.
///
/// Two checks:
///   1. `snitchos.deflake.ipi_pong_sends == N` — hart 0 completed the
///      loop. Anything less means hart 0 wedged or deadlocked mid-loop.
///   2. `snitchos.ipi.received_total >= N / 2` — hart 1 actually
///      handled at least half the IPIs (the rest may have coalesced
///      under pacing jitter). If the value stays small, hart 1 wedged
///      on its pickup path.
///
/// See `plans/residual-race-investigation.md` appendix A.
pub fn deflake_ipi_pong() -> Result<(), String> {
    let mut h = Harness::spawn_with_features(
        "deflake-ipi-pong",
        &["deflake-ipi-pong"],
    )?;

    h.wait_for(SEC * 30, |f, strings| match f {
        OwnedFrame::Metric { name_id, value, .. } => {
            strings.get(name_id).map(String::as_str)
                == Some("snitchos.deflake.ipi_pong_sends")
                && *value >= 10_000
        }
        _ => false,
    })
    .ok_or(
        "snitchos.deflake.ipi_pong_sends never reached 10000 within \
         30s — hart 0 did not finish the IPI loop; deadlock or wedge \
         on hart 0 (likely shared static or symmetric load-side flake).",
    )?;

    h.wait_for(SEC * 10, |f, strings| match f {
        OwnedFrame::Metric { name_id, value, .. } => {
            strings.get(name_id).map(String::as_str)
                == Some("snitchos.ipi.received_total")
                && *value >= 5_000
        }
        _ => false,
    })
    .ok_or(
        "snitchos.ipi.received_total never reached 5000 within 10s \
         after the send loop finished — hart 1 stopped processing IPIs \
         partway through. This is the residual race signature on the \
         post-sret pickup path.",
    )?;
    Ok(())
}

/// Tight `mmu::shootdown(va)` storm from hart 0 to hart 1. Each
/// iteration: hart 0 writes shootdown_va, sends IPI_TLB_SHOOTDOWN,
/// spin-waits on shootdown_ack; hart 1's IPI handler does the
/// Acquire-swap, reads the va, sfences, Release-bumps the ack.
/// Tests the IPI payload-read path — a different surface from
/// `deflake-ipi-pong` (no payload).
///
/// Asserts both:
///   1. `snitchos.deflake.shootdown_storm_sends == N` — hart 0
///      completed the loop. Below N means hart 0 wedged on its
///      built-in Acquire spin (symmetric load-side flake) OR hart 1
///      stopped acking.
///   2. `snitchos.mmu.shootdowns_received_total >= N - tolerance` —
///      hart 1 actually handled the shootdowns. (Per-iteration ack
///      means coalescing shouldn't happen here, unlike ipi-pong.)
pub fn deflake_shootdown_storm() -> Result<(), String> {
    let mut h = Harness::spawn_with_features(
        "deflake-shootdown-storm",
        &["deflake-shootdown-storm"],
    )?;

    h.wait_for(SEC * 30, |f, strings| match f {
        OwnedFrame::Metric { name_id, value, .. } => {
            strings.get(name_id).map(String::as_str)
                == Some("snitchos.deflake.shootdown_storm_sends")
                && *value >= 5_000
        }
        _ => false,
    })
    .ok_or(
        "snitchos.deflake.shootdown_storm_sends never reached 5000 \
         within 30s — hart 0 did not finish the shootdown loop. Either \
         hart 0 wedged on its Acquire spin-wait of shootdown_ack \
         (symmetric load-side flake) or hart 1 stopped acking.",
    )?;

    h.wait_for(SEC * 10, |f, strings| match f {
        OwnedFrame::Metric { name_id, value, .. } => {
            strings.get(name_id).map(String::as_str)
                == Some("snitchos.mmu.shootdowns_received_total")
                && *value >= 4_900
        }
        _ => false,
    })
    .ok_or(
        "snitchos.mmu.shootdowns_received_total never reached 4900 \
         within 10s after the send loop finished — hart 1 silently \
         skipped some shootdowns or its IPI handler is broken.",
    )?;
    Ok(())
}

/// v0.5.x minimal task-exit: a spawned task can call `sched::exit_now`
/// and the kernel keeps running. The boot path spawns `exit_smoke`,
/// whose body bumps `EXIT_SMOKE_HITS` and calls `exit_now`. The
/// asm `switch_into` loads the next ready context (idle) and `ret`s
/// into it without saving the exiting task's registers.
///
/// Asserts `snitchos.sched.exit_smoke_hits == 1` within 30 s.
/// Passing this proves: state flip to `Exited`, runqueue dispatch,
/// asm `switch_into` correctness, and the exiting task's stack being
/// abandoned cleanly (no scribble, no fault).
pub fn sched_task_exits_cleanly() -> Result<(), String> {
    let mut h = Harness::spawn("sched-exit")?;

    h.wait_for(SEC * 30, |f, strings| match f {
        OwnedFrame::Metric { name_id, value, .. } => {
            strings.get(name_id).map(String::as_str)
                == Some("snitchos.sched.exit_smoke_hits")
                && *value >= 1
        }
        _ => false,
    })
    .ok_or(
        "snitchos.sched.exit_smoke_hits never reached 1 within 30s — \
         exit smoke task didn't run, or `exit_now` faulted, or the \
         kernel hung after the asm switch_into.",
    )?;
    Ok(())
}

/// Mutex-contention storm: both harts run a long-running task that
/// takes and releases the same `kernel::sync::Mutex<()>` N=100 000
/// times. Tests revised-H7 — is the cross-hart bug inside
/// `spin::Mutex`'s Acquire/Release pair on multi-thread TCG?
///
/// Asserts both `snitchos.deflake.mutex_storm_acquires_hart0` and
/// `snitchos.deflake.mutex_storm_acquires_hart1` reach N within
/// 30 s. With fix on (trap-return BQL fence) the storm should
/// complete cleanly. With fix off, if revised-H7 is right one or
/// both counters stall mid-loop; the kernel either wedges or one
/// task never advances. See `plans/residual-race-investigation.md`
/// appendix C.
pub fn deflake_mutex_storm() -> Result<(), String> {
    let mut h = Harness::spawn_with_features(
        "deflake-mutex-storm",
        &["deflake-mutex-storm"],
    )?;

    h.wait_for(SEC * 30, |f, strings| match f {
        OwnedFrame::Metric { name_id, value, .. } => {
            strings.get(name_id).map(String::as_str)
                == Some("snitchos.deflake.mutex_storm_acquires_hart0")
                && *value >= 100_000
        }
        _ => false,
    })
    .ok_or(
        "snitchos.deflake.mutex_storm_acquires_hart0 never reached \
         100000 within 30s — hart 0's mutex storm task didn't \
         complete its loop; likely revised-H7 fired (Acquire on \
         spin::Mutex lock dropped under multi-thread TCG).",
    )?;

    h.wait_for(SEC * 30, |f, strings| match f {
        OwnedFrame::Metric { name_id, value, .. } => {
            strings.get(name_id).map(String::as_str)
                == Some("snitchos.deflake.mutex_storm_acquires_hart1")
                && *value >= 100_000
        }
        _ => false,
    })
    .ok_or(
        "snitchos.deflake.mutex_storm_acquires_hart1 never reached \
         100000 within 30s — hart 1's mutex storm task didn't \
         complete its loop. Same diagnosis as hart 0.",
    )?;
    Ok(())
}

/// Virtio-emit storm. Hart 0 calls `tracing::emit_metric` in a tight
/// loop (each call: intern check + frame serialize + TX_STAGING.lock +
/// virtio descriptor + MMIO notify). Hart 1 does pure Relaxed
/// `fetch_add` on a shared atomic. No cross-hart mutex contention.
///
/// Tests H11-refined: is the cross-hart bug specifically inside the
/// virtio TX path? With fix on, BQL fences at every trap return
/// should keep this clean. With fix off, if H11-refined is right,
/// hart 0 should wedge mid-emit and the counter stalls.
///
/// Asserts `snitchos.deflake.virtio_storm_hart0_emits` reaches N
/// (5 000) within 30 s. See `plans/residual-race-investigation.md`
/// appendix C.
pub fn deflake_virtio_storm() -> Result<(), String> {
    let mut h = Harness::spawn_with_features(
        "deflake-virtio-storm",
        &["deflake-virtio-storm"],
    )?;

    h.wait_for(SEC * 30, |f, strings| match f {
        OwnedFrame::Metric { name_id, value, .. } => {
            strings.get(name_id).map(String::as_str)
                == Some("snitchos.deflake.virtio_storm_hart0_emits")
                && *value >= 5_000
        }
        _ => false,
    })
    .ok_or(
        "snitchos.deflake.virtio_storm_hart0_emits never reached \
         5000 within 30s — hart 0's emit loop didn't finish; \
         likely H11-refined fired (virtio TX path bug).",
    )?;
    Ok(())
}
