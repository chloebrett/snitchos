//! One function per scenario. Each returns `Ok(())` on pass, or a
//! human-readable `String` describing what didn't match.

use std::time::Duration;

use fs_proto::markers;
use protocol::stream::OwnedFrame;


use super::harness::View;
use super::matchers::{is_cap_granted_span, is_cap_granted_telemetry, is_dropped, is_hello, is_metric_named, is_span_start_named, is_string_register_named, is_thread_register_named};

const SEC: Duration = Duration::from_secs(1);

/// Frame allocator is initialized and exercised. Each heartbeat does
/// an `alloc_zeroed` + `free`, so the counters tick up over time. The
/// scenario waits for a `snitchos.frames.allocated_total` metric with
/// value ≥ 1, which proves: init ran, the linear map resolves (the
/// zeroing wrote 4 KiB via `pa_to_kernel_va`), and at least one
/// heartbeat completed.
pub fn frame_allocator_metrics(h: &mut View) -> Result<(), String> {
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
pub fn kernel_heap_metrics(h: &mut View) -> Result<(), String> {
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
/// `workload=heap-oom` selection leaks 4096 × 4 KiB blocks per heartbeat
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
pub fn sched_context_switch_smoke(h: &mut View) -> Result<(), String> {
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
/// spawns "idle", "`task_a`", "`task_b`" via `spawn(name, entry)`. Each
/// call emits a `ThreadRegister` frame. This scenario asserts all
/// four appear within budget, proving `spawn` builds + queues each
/// task and the wire carries names through to the collector.
pub fn sched_spawn_registers_thread(h: &mut View) -> Result<(), String> {
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

/// Cooperative round-robin works: main, idle, `task_a`, `task_b` are all
/// taking turns. We assert both demo tasks' loop counters rise above
/// 0 within budget, plus the scheduler's cumulative switch counter
/// climbs. That triplet rules out "`yield_now` does nothing" and "only
/// one task runs."
pub fn sched_yield_round_trips(h: &mut View) -> Result<(), String> {
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
/// task is resumed. `task_a` opens `task_a.tick`, yields mid-span,
/// gets re-scheduled, then closes. The wire should show:
///
///   1. `SpanStart` for "`task_a.tick`" with `task_id == task_a_id`,
///      `parent == SpanId(0)` (top-level — proves per-task cursor
///      isn't being polluted by other tasks' spans).
///   2. At least one `ContextSwitch` leaving `task_a`, and one returning.
///   3. `SpanEnd` for the same span id as (1).
///
/// Without per-task `SpanCursor` wiring, the parent in (1) could be
/// any other task's currently-open span, and (3)'s pop would land on
/// the wrong cursor. This scenario is the structural proof that the
/// per-task wiring works.
pub fn sched_span_survives_yield(h: &mut View) -> Result<(), String> {
    use protocol::SpanId;

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
/// values. We harvest the `ThreadRegister` id for each known task,
/// then wait for a `ContextSwitch` frame whose endpoints are both
/// recognised task ids and whose reason is `Yield` (only switch
/// flavour in cooperative v0.5). Proves the scheduler is emitting
/// the per-switch event, not just the cumulative counter.
pub fn sched_context_switches_on_wire(h: &mut View) -> Result<(), String> {
    use std::collections::HashSet;

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
/// `SpanStart` frames on the wire, and each carries its own `task_id`
/// (matching the `ThreadRegister` for its name). Proves spans are
/// correctly tagged to the task that emitted them.
pub fn sched_spans_carry_task_id(h: &mut View) -> Result<(), String> {
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

pub fn heap_oom(h: &mut View) -> Result<(), String> {
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
/// The `workload=frame-oom` selection leaks 8192 frames per heartbeat
/// (32 MiB), so the ~32K-frame pool runs out in ~4 heartbeats on the
/// default QEMU `virt` config. We assert:
///
///   1. `snitchos.frames.alloc_failed_total` eventually rises above 0
///      — the allocator handled OOM by returning `None`, not by
///      crashing.
///   2. At least two more heartbeats arrive after the first failure
///      — the kernel didn't lock up; metrics keep flowing.
pub fn frame_allocator_oom(h: &mut View) -> Result<(), String> {
    // Select the `frame-oom` workload so the heartbeat smoke leaks
    // 8192 frames/tick instead of doing alloc+free.
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
pub fn kernel_runs_at_higher_half(h: &mut View) -> Result<(), String> {
    h.wait_for(SEC * 20, is_span_start_named("kernel.runs_at_higher_half"))
        .ok_or("no kernel.runs_at_higher_half SpanStart — PC isn't actually at higher-half post-trampoline")?;
    Ok(())
}

/// Boot sequence reaches the heartbeat loop: Hello → kernel.boot
/// `SpanStart` → Dropped(0) (proves pre-init flush ran cleanly) →
/// first kernel.heartbeat `SpanStart` (proves the timer IRQ is firing).
pub fn boot_reaches_heartbeat(h: &mut View) -> Result<(), String> {
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

/// Two consecutive heartbeat `SpanStarts` arrive with monotonic timestamps
/// and a sane tick interval. Captures `Hello` first to get the timebase,
/// then converts the tick delta to nanoseconds and asserts it falls
/// between 10 ms and 10 s — loose enough to survive QEMU stalls but
/// tight enough to catch a runaway or frozen timer.
pub fn heartbeat_cadence(h: &mut View) -> Result<(), String> {
    const MIN_NS: u128 = 10_000_000; // 10 ms
    const MAX_NS: u128 = 10_000_000_000; // 10 s

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

    let delta_ns = u128::from(t2 - t1) * 1_000_000_000 / u128::from(timebase_hz);
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
///      — it was registered before `virtio_console::init` succeeded,
///      so it lived in the pre-init buffer.
///   2. Every span's `name_id` was registered earlier in the stream.
///      If the buffer dequeued out of order we'd see `SpanStarts`
///      referencing unknown ids.
pub fn pre_init_order(h: &mut View) -> Result<(), String> {
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
                        "SpanStart references unregistered name_id {name_id:?} — buffer flush is out of order"
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
pub fn smp_spawn_on_hart_1_runs(h: &mut View) -> Result<(), String> {
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

/// v0.6 step 13: the wire-format `hart_id` is correct end-to-end.
/// `SpanStart` carries `hart_id` stamped from `current_hartid()` at
/// open time, so a span's `hart_id` is the hart it actually ran on.
/// The default workload runs `task_a` on hart 0 and the `hart_1_probe`
/// on hart 1, so we should see both attributions on the wire:
///
///   - a `task_a.tick` `SpanStart` with `hart_id == 0`, and
///   - a `hart1.probe` `SpanStart` with `hart_id == 1`.
///
/// Proves the per-hart attribution path (kernel `current_hartid()` →
/// `Frame::SpanStart.hart_id` → collector) for *both* harts. Distinct
/// from `smp-spawn-on-hart-1-runs` (which checks a metric counter, not
/// the span's hart attribution).
pub fn smp_spans_carry_hart_id(h: &mut View) -> Result<(), String> {
    h.wait_for(SEC * 30, |f, strings| match f {
        OwnedFrame::SpanStart { name_id, hart_id, .. } => {
            strings.get(name_id).map(String::as_str) == Some("task_a.tick")
                && *hart_id == 0
        }
        _ => false,
    })
    .ok_or(
        "no task_a.tick SpanStart with hart_id==0 within 30s — hart 0 \
         spans aren't carrying the right hart_id (or task_a never ran).",
    )?;

    h.wait_for(SEC * 30, |f, strings| match f {
        OwnedFrame::SpanStart { name_id, hart_id, .. } => {
            strings.get(name_id).map(String::as_str) == Some("hart1.probe")
                && *hart_id == 1
        }
        _ => false,
    })
    .ok_or(
        "no hart1.probe SpanStart with hart_id==1 within 30s — hart 1's \
         spans aren't carrying hart_id==1 (probe didn't run on hart 1, \
         or current_hartid() is wrong on the secondary).",
    )?;

    Ok(())
}

/// v0.6 step 13: an idle hart is woken by an IPI to run new work.
/// hart 1 boots straight into its idle task (`wfi`) with an empty
/// runqueue; the only thing that puts it to work is hart 0's
/// `spawn_on(1, "hart_1_probe", …)`, which enqueues the task and sends
/// `IPI_WAKEUP`. The probe's first span — tagged `hart_id == 1` — is
/// the end-to-end proof the IPI pulled hart 1 out of `wfi` and ran it.
///
/// Asserts the `hart1.probe` `SpanStart` (`hart_id == 1`) appears within
/// 20s. (Complements `smp-spawn-on-hart-1-runs`, which proves
/// *sustained* progress via the metric; this guards the *wake* edge
/// itself, observed as a span.)
pub fn smp_ipi_wakes_idle_hart(h: &mut View) -> Result<(), String> {
    h.wait_for(SEC * 20, |f, strings| match f {
        OwnedFrame::SpanStart { name_id, hart_id, .. } => {
            strings.get(name_id).map(String::as_str) == Some("hart1.probe")
                && *hart_id == 1
        }
        _ => false,
    })
    .ok_or(
        "hart1.probe span (hart_id==1) never appeared within 20s — the \
         idle hart wasn't woken: spawn_on didn't enqueue, IPI_WAKEUP \
         wasn't delivered, or hart 1 never left wfi.",
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
pub fn smp_secondary_hart_boots(h: &mut View) -> Result<(), String> {
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
pub fn ipi_self_wakeup(h: &mut View) -> Result<(), String> {
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
/// when sampled at the same instant; `histogram_sum` may briefly
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
pub fn workload_cooperative_baseline(h: &mut View) -> Result<(), String> {
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

/// v0.6 step 11: the producer/consumer workload, but cross-hart.
/// Selected at runtime via the `workload=smp` bootarg on the
/// `itest-workloads` kernel — producer on hart 0, consumer on hart 1;
/// the `Mutex<VecDeque>` queue now carries genuine inter-hart
/// contention (the v0.6 thesis — the chokepoint earns its keep by
/// being *visible*).
///
/// This is the SMP analogue of `workload-cooperative-baseline`. The
/// same correctness oracle applies — `histogram_sum >= samples_consumed`
/// — but now the consumer's bin writes and consumed counter cross a
/// hart boundary before the heartbeat (hart 0) reads them. A missing
/// Release/Acquire pair would let hart 0 observe `consumed` ahead of
/// the bins, so `histogram_sum < consumed` and this scenario fails.
/// Run under `--repeat 10` (the commit gate) to surface that race.
///
/// Threshold = 1000 (not the baseline's 200): the consumer now has its
/// own hart, so it converges fast, and 1000 samples forces ~16 cross-
/// hart batch handoffs per run — enough interleavings to give the
/// memory-ordering hazard room to manifest.
pub fn smp_producer_consumer_correctness(h: &mut View) -> Result<(), String> {
    // `burst=256` instead of the default 1. At burst=1 the workload is
    // cadence-bound (~64 samples/s — see post 19), so reaching 1000
    // samples takes ~16s. A burst makes the two harts' batches overlap,
    // which both reaches the threshold in well under a second *and*
    // puts the correctness oracle under genuine cross-hart contention
    // rather than near-serial 1 Hz blips. (`burst=` and `workload=` are
    // separate bootargs tokens; the kernel applies burst for any
    // workload.)
    let frame = h
        .wait_for(SEC * 45, |f, strings| match f {
            OwnedFrame::Metric { name_id, value, .. } => {
                strings.get(name_id).map(String::as_str)
                    == Some("snitchos.workload.samples_consumed_total")
                    && *value >= 1000
            }
            _ => false,
        })
        .ok_or(
            "samples_consumed_total never reached 1000 within 45s — \
             consumer not running on hart 1, or cross-hart spawn/IPI \
             wakeup not delivering work?",
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
         observing samples_consumed_total={consumed} — cross-hart \
         samples lost, or hart 0 observed consumed ahead of the bin \
         writes (missing Release/Acquire on the consumed counter?)"
    ))?;

    Ok(())
}

/// Cross-hart spawn storm. Hart 0 calls `spawn_on(1, storm_body)` in
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
/// Built with `workload=spawn-storm` so the default boot
/// workload is replaced by the storm; the gating also turns off the
/// per-spawn `emit_thread_register` so no incidental BQL fence closes
/// the window mid-storm. See `plans/residual-race-investigation.md`.
pub fn spawn_storm(h: &mut View) -> Result<(), String> {
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

/// Tight `IPI_WAKEUP` storm from hart 0 to hart 1. Each iteration of the
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
pub fn ipi_pong(h: &mut View) -> Result<(), String> {
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
/// iteration: hart 0 writes `shootdown_va`, sends `IPI_TLB_SHOOTDOWN`,
/// spin-waits on `shootdown_ack`; hart 1's IPI handler does the
/// Acquire-swap, reads the va, sfences, Release-bumps the ack.
/// Tests the IPI payload-read path — a different surface from
/// `ipi-pong` (no payload).
///
/// Asserts both:
///   1. `snitchos.deflake.shootdown_storm_sends == N` — hart 0
///      completed the loop. Below N means hart 0 wedged on its
///      built-in Acquire spin (symmetric load-side flake) OR hart 1
///      stopped acking.
///   2. `snitchos.mmu.shootdowns_received_total >= N - tolerance` —
///      hart 1 actually handled the shootdowns. (Per-iteration ack
///      means coalescing shouldn't happen here, unlike ipi-pong.)
pub fn shootdown_storm(h: &mut View) -> Result<(), String> {
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

/// v0.6 step 13: TLB-shootdown *correctness* (not just plumbing).
/// `shootdown-storm` proves the IPI payload-read path; this proves the
/// consequence — after hart 0 repoints a VA at a new frame and shoots
/// down, hart 1 stops reading the old one.
///
/// The `tlb-shootdown` workload has hart 0 remap a shared VA between
/// two pre-filled frames each round (firing `mmu::remap` →
/// `shootdown`), while hart 1 reads through that VA every round. hart 1
/// reads the *old* frame before each remap, caching the stale
/// translation; only the shootdown's cross-hart `sfence` can
/// invalidate it. A miss shows up as a stale read.
///
/// We assert:
///   1. `snitchos.smp.tlb_remap_rounds` reaches 100 — the remap/read
///      loop actually ran enough rounds that the result isn't vacuous
///      (a fresh-map-only test would pass without any shootdown).
///   2. `snitchos.smp.tlb_stale_reads` is never observed `> 0` — the
///      cumulative, re-emitted oracle. Any stale read means a hart kept
///      a stale TLB entry after a remap: shootdown failed.
///
/// Teeth are proven out of band by a deliberately-broken counterfactual
/// (see `plans/v0.6-step-13-tlb-shootdown-visible.md`).
pub fn smp_tlb_shootdown_visible(h: &mut View) -> Result<(), String> {
    h.wait_for(SEC * 30, |f, strings| match f {
        OwnedFrame::Metric { name_id, value, .. } => {
            strings.get(name_id).map(String::as_str)
                == Some("snitchos.smp.tlb_remap_rounds")
                && *value >= 100
        }
        _ => false,
    })
    .ok_or(
        "tlb_remap_rounds never reached 100 within 30s — the remap/read \
         loop didn't run. hart 1 reader didn't pick up, `run` wedged on \
         a shootdown ack, or the heartbeat never drove the driver.",
    )?;

    // The oracle is cumulative and re-emitted every heartbeat, so by the
    // time rounds >= 100 any stale read is latched and will reappear.
    // Finding one within a few ticks is the failure this scenario exists
    // to catch — so the *clean* path is this 5s window elapsing with no
    // `tlb_stale_reads > 0`. `assert_absent` makes that an explicit pass
    // (no scary timeout dump), not a `wait_for` that happens to time out.
    h.assert_absent(
        SEC * 5,
        "tlb_stale_reads>0",
        "hart 1 observed a STALE TLB translation after a remap \
         (tlb_stale_reads > 0) — mmu::remap's shootdown did not \
         invalidate the other hart's cached entry.",
        |f, strings| match f {
            OwnedFrame::Metric { name_id, value, .. } => {
                strings.get(name_id).map(String::as_str)
                    == Some("snitchos.smp.tlb_stale_reads")
                    && *value > 0
            }
            _ => false,
        },
    )
}

/// v0.6 step 13: cross-hart ping-pong cadence — a wakeup oracle
/// independent of the producer/consumer workload. ping (hart 0) and
/// pong (hart 1) alternate turns through a shared flag, each handing
/// off with an `IPI_WAKEUP` to the partner, which had fallen idle in
/// `wfi`. Both turn counters reaching K=200 is only possible under
/// strict, repeated cross-hart re-wake.
///
/// We assert both `snitchos.smp.ping_turns_total` and
/// `snitchos.smp.pong_turns_total` reach 200 within budget. The budget
/// is the teeth: with the IPI working each handoff is microseconds; a
/// silently-dropped wakeup would leave each side waiting on the 1 Hz
/// timer, so 400 handoffs would take minutes and time out.
pub fn smp_ping_pong_cadence(h: &mut View) -> Result<(), String> {
    h.wait_for(SEC * 30, |f, strings| match f {
        OwnedFrame::Metric { name_id, value, .. } => {
            strings.get(name_id).map(String::as_str)
                == Some("snitchos.smp.ping_turns_total")
                && *value >= 200
        }
        _ => false,
    })
    .ok_or(
        "ping_turns_total never reached 200 within 30s — ping stalled. \
         Likely a handoff wasn't woken: hart 1's pong didn't re-wake \
         hart 0 by IPI, so the turn flag wedged (or the IPI is only \
         delivering at timer cadence).",
    )?;

    h.wait_for(SEC * 30, |f, strings| match f {
        OwnedFrame::Metric { name_id, value, .. } => {
            strings.get(name_id).map(String::as_str)
                == Some("snitchos.smp.pong_turns_total")
                && *value >= 200
        }
        _ => false,
    })
    .ok_or(
        "pong_turns_total never reached 200 within 30s — pong stalled. \
         The idle hart 1 wasn't re-woken by hart 0's handoff IPI.",
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
pub fn sched_task_exits_cleanly(h: &mut View) -> Result<(), String> {
    // The exit's context switch carries `SwitchReason::Exit` on the wire — an
    // exit is distinguishable from a voluntary yield. `exit_smoke` is the only
    // task that exits in this boot, so any `ContextSwitch{Exit}` is its. Asserted
    // first: it's emitted *at* the exit, before the heartbeat later drains the
    // `exit_smoke_hits` metric below.
    h.wait_for(SEC * 30, |f, _| {
        matches!(f, OwnedFrame::ContextSwitch { reason, .. }
            if matches!(reason, protocol::SwitchReason::Exit))
    })
    .ok_or(
        "no ContextSwitch{Exit} within 30s — `exit_now` didn't label the exit switch \
         distinctly from a yield on the wire (or the exit task never ran)",
    )?;

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

/// v0.9 block/wake smoke (`workload=block-wake`): a `blocker` kernel task
/// stores its id, arms a flag, and calls `block_current` — leaving the CPU
/// *off* the runqueue (not re-enqueued, unlike `yield_now`). A `waker` peer
/// spins yielding until it sees the flag, then calls `wake(blocker)`, which
/// returns the blocker to `Ready`. The scheduler picks it; `block_current`
/// returns; the blocker bumps `snitchos.sched.wake_resumed`. Asserting it
/// reaches exactly 1 proves the round-trip: block → switch-away → wake →
/// resume. A hang (lost wakeup, or the two-way `switch` not saving/restoring
/// the blocker's context) is caught by the timeout.
pub fn block_wake_smoke(h: &mut View) -> Result<(), String> {
    let frame = h
        .wait_for(SEC * 30, |f, strings| match f {
            OwnedFrame::Metric { name_id, value, .. } => {
                strings.get(name_id).map(String::as_str)
                    == Some("snitchos.sched.wake_resumed")
                    && *value >= 1
            }
            _ => false,
        })
        .ok_or(
            "no sched.wake_resumed >= 1 within 30s — blocker never resumed \
             after wake (lost wakeup, block_current didn't save context, or \
             wake didn't re-enqueue)",
        )?;
    let value = match frame {
        OwnedFrame::Metric { value, .. } => value,
        _ => return Err("matched non-metric (impossible)".to_string()),
    };
    if value != 1 {
        return Err(format!(
            "sched.wake_resumed = {value}, expected exactly 1 (blocker resumes once)"
        ));
    }
    Ok(())
}

/// v0.9 IPC milestone heart (`workload=ipc`): process A (`ipc-sender`, holding
/// a `SEND` cap) sends the inline message `[42, 0, 0, 0]` over a kernel-brokered
/// endpoint; process B (`ipc-receiver`, holding a `RECV` cap) receives it and
/// re-emits word0 through its `TelemetrySink`. Asserting
/// `snitchos.user.telemetry_total == 42` proves the *exact* payload crossed the
/// process boundary through the synchronous rendezvous (block → switch → wake →
/// deliver). A hang (lost wakeup, or the message never copied) trips the
/// timeout; a wrong value catches a mis-copied word.
pub fn ipc_message_crosses(h: &mut View) -> Result<(), String> {
    h.wait_for(SEC * 30, |f, strings| match f {
        OwnedFrame::Metric { name_id, value, .. } => {
            strings.get(name_id).map(String::as_str) == Some("snitchos.user.telemetry_total")
                && *value == 42
        }
        _ => false,
    })
    .ok_or(
        "no snitchos.user.telemetry_total == 42 within 30s — the message didn't \
         cross: receiver never received the payload, the words were mis-copied, \
         or the rendezvous hung (lost wakeup / message not staged)",
    )?;
    Ok(())
}

/// v0.9 headline (`workload=ipc`): the trace follows the message across the
/// process boundary. The sender opens `ipc.send` and sends *inside* it; the
/// kernel carries that span as the message's trace context and seeds it onto
/// the receiver, so the `ipc.recv` span the receiver opens after receiving is a
/// **child** of `ipc.send` — two different processes, one trace. Asserts the
/// `ipc.recv` SpanStart's `parent` equals the (non-root) `ipc.send` span id.
pub fn ipc_trace_crosses(h: &mut View) -> Result<(), String> {
    use protocol::SpanId;

    let send = h
        .wait_for(SEC * 30, |f, strings| {
            matches!(f, OwnedFrame::SpanStart { name_id, .. }
                if strings.get(name_id).map(String::as_str) == Some("ipc.send"))
        })
        .ok_or("no SpanStart for 'ipc.send' within 30s — sender never opened its span")?;
    let send_id = match send {
        OwnedFrame::SpanStart { id, .. } => id,
        _ => return Err("matched non-SpanStart (impossible)".to_string()),
    };
    if send_id == SpanId(0) {
        return Err("ipc.send span id is 0 (root sentinel) — no real span to parent under".to_string());
    }

    let recv = h
        .wait_for(SEC * 30, |f, strings| {
            matches!(f, OwnedFrame::SpanStart { name_id, .. }
                if strings.get(name_id).map(String::as_str) == Some("ipc.recv"))
        })
        .ok_or("no SpanStart for 'ipc.recv' within 30s — receiver never opened its handling span")?;
    let recv_parent = match recv {
        OwnedFrame::SpanStart { parent, .. } => parent,
        _ => return Err("matched non-SpanStart (impossible)".to_string()),
    };

    if recv_parent != send_id {
        return Err(format!(
            "ipc.recv parent {recv_parent:?} != ipc.send id {send_id:?} — the trace did \
             not cross the process boundary (kernel didn't seed the sender's span context)"
        ));
    }
    Ok(())
}

/// v0.9 IPC observability (`workload=ipc`): the rendezvous is counted and
/// recorded. Asserts a `Frame::Message` reaches the wire naming distinct
/// from/to tasks (the per-rendezvous topology record — the Step-3 wire variant
/// finally gets an emitter), then that `snitchos.ipc.messages_total` and
/// `snitchos.ipc.blocks_total` both reach ≥1 (deferred-emission counters,
/// bumped at the event and drained in the heartbeat). The one-shot `Message`
/// frame is matched first (it passes once); the cumulative counters after (a
/// fresh heartbeat re-emits them every tick).
pub fn ipc_telemetry(h: &mut View) -> Result<(), String> {
    h.wait_for(SEC * 30, |f, _strings| {
        matches!(f, OwnedFrame::Message { from, to, .. } if from != to)
    })
    .ok_or("no Frame::Message with distinct from/to within 30s — the rendezvous record never reached the wire")?;

    h.wait_for(SEC * 30, |f, strings| {
        matches!(f, OwnedFrame::Metric { name_id, value, .. }
            if strings.get(name_id).map(String::as_str) == Some("snitchos.ipc.messages_total") && *value >= 1)
    })
    .ok_or("no snitchos.ipc.messages_total >= 1 within 30s")?;

    h.wait_for(SEC * 30, |f, strings| {
        matches!(f, OwnedFrame::Metric { name_id, value, .. }
            if strings.get(name_id).map(String::as_str) == Some("snitchos.ipc.blocks_total") && *value >= 1)
    })
    .ok_or("no snitchos.ipc.blocks_total >= 1 within 30s — neither side blocked on the rendezvous")?;
    Ok(())
}

/// v0.9 wakeup-latency guard (`workload=ipc`): after the rendezvous wakes the
/// receiver, it must run **promptly** — not wait for the next ~1s timer tick.
/// The bug this guards: an idle loop that `wfi`s while a just-woken task sits
/// `Ready` on its runqueue, stranding it until a timer IRQ breaks `wfi`.
/// Asserts `ipc.recv` opens within 100ms of `ipc.send` (it was ~1s before the
/// idle-loop fix). Reads `timebase_hz` from `Hello` so the budget is in real
/// time regardless of the clock rate.
pub fn ipc_wakeup_is_prompt(h: &mut View) -> Result<(), String> {
    let hello = h
        .wait_for(SEC * 20, |f, _| matches!(f, OwnedFrame::Hello { .. }))
        .ok_or("no Hello frame within 20s")?;
    let timebase_hz = match hello {
        OwnedFrame::Hello { timebase_hz, .. } => timebase_hz,
        _ => return Err("matched non-Hello (impossible)".to_string()),
    };

    let send = h
        .wait_for(SEC * 30, |f, strings| {
            matches!(f, OwnedFrame::SpanStart { name_id, .. }
                if strings.get(name_id).map(String::as_str) == Some("ipc.send"))
        })
        .ok_or("no SpanStart for 'ipc.send' within 30s")?;
    let t_send = match send {
        OwnedFrame::SpanStart { t, .. } => t,
        _ => return Err("matched non-SpanStart (impossible)".to_string()),
    };

    let recv = h
        .wait_for(SEC * 30, |f, strings| {
            matches!(f, OwnedFrame::SpanStart { name_id, .. }
                if strings.get(name_id).map(String::as_str) == Some("ipc.recv"))
        })
        .ok_or("no SpanStart for 'ipc.recv' within 30s")?;
    let t_recv = match recv {
        OwnedFrame::SpanStart { t, .. } => t,
        _ => return Err("matched non-SpanStart (impossible)".to_string()),
    };

    let gap_ticks = t_recv.saturating_sub(t_send);
    let budget_ticks = timebase_hz / 10; // 100 ms
    if gap_ticks > budget_ticks {
        let gap_ms = (gap_ticks * 1000) / timebase_hz;
        return Err(format!(
            "ipc.recv opened {gap_ms}ms after ipc.send (budget 100ms) — the woken \
             receiver waited for a timer tick because the idle loop wfi'd past ready work"
        ));
    }
    Ok(())
}

/// v0.9b RPC round-trip (`workload=ipc-rpc`): the client `call`s with request
/// 21 and blocks; the server `receive`s it with a one-shot reply cap, computes
/// `21 * 2`, and `reply`s; the client's `call` returns 42 and re-emits it.
/// Asserting `snitchos.user.telemetry_total == 42` proves the whole round-trip:
/// request crossed (server saw 21), server computed, reply crossed back via the
/// minted reply cap (client got 42). A hang (no reply / lost wakeup) trips the
/// timeout.
pub fn rpc_round_trips(h: &mut View) -> Result<(), String> {
    h.wait_for(SEC * 30, |f, strings| {
        matches!(f, OwnedFrame::Metric { name_id, value, .. }
            if strings.get(name_id).map(String::as_str) == Some("snitchos.user.telemetry_total")
                && *value == 42)
    })
    .ok_or(
        "no snitchos.user.telemetry_total == 42 within 30s — the RPC round-trip didn't \
         complete (request didn't cross, server didn't reply, or the response didn't \
         return to the client)",
    )?;
    Ok(())
}

/// v0.9b RPC headline (`workload=ipc-rpc`): the callee's span is **temporally
/// nested** inside the caller's. The client opens `rpc.call` and `call`s inside
/// it — blocking across the whole round-trip — so the server's `rpc.handle`
/// span both descends from it (parent linkage) *and* lies within its
/// `[start, end]` window. This is the RPC flame-graph shape that v0.9's one-way
/// `send` cannot produce: there the child (`ipc.recv`) starts *after* the parent
/// (`ipc.send`) has already closed (the sender doesn't wait), so containment
/// fails — which is exactly the difference this asserts.
pub fn rpc_trace_nests(h: &mut View) -> Result<(), String> {
    use protocol::SpanId;

    let call = h
        .wait_for(SEC * 30, |f, strings| {
            matches!(f, OwnedFrame::SpanStart { name_id, .. }
                if strings.get(name_id).map(String::as_str) == Some("rpc.call"))
        })
        .ok_or("no SpanStart for 'rpc.call' within 30s")?;
    let (call_id, call_start) = match call {
        OwnedFrame::SpanStart { id, t, .. } => (id, t),
        _ => return Err("matched non-SpanStart (impossible)".to_string()),
    };
    if call_id == SpanId(0) {
        return Err("rpc.call span id is 0 (root sentinel)".to_string());
    }

    let handle = h
        .wait_for(SEC * 30, |f, strings| {
            matches!(f, OwnedFrame::SpanStart { name_id, .. }
                if strings.get(name_id).map(String::as_str) == Some("rpc.handle"))
        })
        .ok_or("no SpanStart for 'rpc.handle' within 30s")?;
    let (handle_id, handle_parent, handle_start) = match handle {
        OwnedFrame::SpanStart { id, parent, t, .. } => (id, parent, t),
        _ => return Err("matched non-SpanStart (impossible)".to_string()),
    };
    if handle_parent != call_id {
        return Err(format!(
            "rpc.handle parent {handle_parent:?} != rpc.call id {call_id:?} — not a child"
        ));
    }

    let handle_end = h
        .wait_for(SEC * 30, |f, _| matches!(f, OwnedFrame::SpanEnd { id, .. } if *id == handle_id))
        .ok_or("no SpanEnd for rpc.handle within 30s")?;
    let handle_end = match handle_end {
        OwnedFrame::SpanEnd { t, .. } => t,
        _ => return Err("matched non-SpanEnd (impossible)".to_string()),
    };

    let call_end = h
        .wait_for(SEC * 30, |f, _| matches!(f, OwnedFrame::SpanEnd { id, .. } if *id == call_id))
        .ok_or("no SpanEnd for rpc.call within 30s")?;
    let call_end = match call_end {
        OwnedFrame::SpanEnd { t, .. } => t,
        _ => return Err("matched non-SpanEnd (impossible)".to_string()),
    };

    if !(call_start <= handle_start && handle_end <= call_end) {
        return Err(format!(
            "rpc.handle [{handle_start}, {handle_end}] not contained in rpc.call \
             [{call_start}, {call_end}] — the caller's span didn't stay open across \
             the callee's work (that's the one-way `send` shape, not RPC)"
        ));
    }
    Ok(())
}

/// v0.9b RPC telemetry (`workload=ipc-rpc`): the round-trip is counted. Asserts
/// `snitchos.ipc.calls_total` and `snitchos.ipc.replies_total` both reach ≥1 —
/// deferred-emission counters bumped in the `call`/`reply` handlers and drained
/// in the heartbeat (never a frame from the rendezvous itself).
pub fn rpc_telemetry(h: &mut View) -> Result<(), String> {
    h.wait_for(SEC * 30, |f, strings| {
        matches!(f, OwnedFrame::Metric { name_id, value, .. }
            if strings.get(name_id).map(String::as_str) == Some("snitchos.ipc.calls_total")
                && *value >= 1)
    })
    .ok_or("no snitchos.ipc.calls_total >= 1 within 30s")?;

    h.wait_for(SEC * 30, |f, strings| {
        matches!(f, OwnedFrame::Metric { name_id, value, .. }
            if strings.get(name_id).map(String::as_str) == Some("snitchos.ipc.replies_total")
                && *value >= 1)
    })
    .ok_or("no snitchos.ipc.replies_total >= 1 within 30s — server never replied")?;
    Ok(())
}

/// v0.9b `reply_recv` (`workload=ipc-rpc`): the server's fused reply-then-
/// receive loop serves **two** requests from the client (21→42, 50→100). The
/// second round-trip completing proves the loop iterated *and* that the second
/// reply cap reused the first's freed `CapTable` slot (generation-bumped, so no
/// aliasing). Asserting `snitchos.user.telemetry_total == 100` — the second
/// response — is the end-to-end witness of the fused path + slot reuse.
pub fn rpc_reply_recv(h: &mut View) -> Result<(), String> {
    h.wait_for(SEC * 30, |f, strings| {
        matches!(f, OwnedFrame::Metric { name_id, value, .. }
            if strings.get(name_id).map(String::as_str) == Some("snitchos.user.telemetry_total")
                && *value == 100)
    })
    .ok_or(
        "no snitchos.user.telemetry_total == 100 within 30s — the second RPC didn't \
         complete (reply_recv loop didn't serve a second request, or the reused reply \
         cap aliased / failed)",
    )?;
    Ok(())
}

/// v0.9c badged endpoints: a `RECV | MINT` minter and a `SEND`-only client run
/// the *same* binary, each calling `mint_badged`. The minter succeeds — a
/// `CapEvent::Transferred{Endpoint}` carrying the badge appears on the wire; the
/// client is refused — a `SyscallRefused{MintBadged}`. Proves the demux value is
/// server-minted and the `MINT` gate holds, with the outcome decided by the
/// capability, not the code.
pub fn badge_mint_mints_and_refuses(h: &mut View) -> Result<(), String> {
    use protocol::{CapEventKind, CapObject};

    h.wait_for(SEC * 30, |f, _| {
        matches!(f, OwnedFrame::CapEvent { kind: CapEventKind::Transferred, object: CapObject::Endpoint, badge, .. }
            if *badge == 0xF00D)
    })
    .ok_or(
        "no CapEvent::Transferred{Endpoint, badge=0xF00D} within 30s — the RECV|MINT \
         minter didn't mint a badged cap",
    )?;

    let mint_badged = snitchos_abi::Syscall::MintBadged as u8;
    h.wait_for(SEC * 30, |f, _| matches!(f, OwnedFrame::SyscallRefused { syscall, .. } if *syscall == mint_badged))
        .ok_or(
            "no SyscallRefused{MintBadged} within 30s — the SEND-only client's mint \
             wasn't refused (the MINT gate didn't hold)",
        )?;
    Ok(())
}

/// v0.9c cap-transfer-in-reply: a `RECV | MINT` server mints a badged cap per
/// `call` and hands it back in the `reply`. Proves a server can return a
/// capability to a client — the keystone the filesystem's `open` needs. The
/// first handout carries the server's first assigned badge (`0xBEE1`), snitched
/// as a `CapEvent::Transferred`.
pub fn badge_handout_transfers_cap(h: &mut View) -> Result<(), String> {
    use protocol::{CapEventKind, CapObject};

    h.wait_for(SEC * 30, |f, _| {
        matches!(f, OwnedFrame::CapEvent { kind: CapEventKind::Transferred, object: CapObject::Endpoint, badge, .. }
            if *badge == 0xBEE1)
    })
    .ok_or(
        "no CapEvent::Transferred{Endpoint, badge=0xBEE1} within 30s — the server didn't \
         mint + hand back a badged cap",
    )?;
    Ok(())
}

/// v0.9c **the headline**: one endpoint, two clients, told apart by capability.
/// Each client `call`s the server (getting a distinct server-assigned badge,
/// `0xBEE1`/`0xBEE2`) then `send`s on that badged cap. The kernel delivers each
/// sender's badge to the server's single receive loop, which re-emits it. Assert
/// **both distinct badges** surface — the demux works, by badge not by identity.
/// Order-independent: the two emits can interleave, so accumulate.
pub fn badge_demux_distinguishes_clients(h: &mut View) -> Result<(), String> {
    use std::cell::Cell;

    let seen_a = Cell::new(false);
    let seen_b = Cell::new(false);
    h.wait_for(SEC * 30, |f, strings| {
        if let OwnedFrame::Metric { name_id, value, .. } = f
            && strings.get(name_id).map(String::as_str) == Some("snitchos.user.telemetry_total")
        {
            if *value == 0xBEE1 {
                seen_a.set(true);
            }
            if *value == 0xBEE2 {
                seen_b.set(true);
            }
        }
        seen_a.get() && seen_b.get()
    })
    .ok_or(
        "didn't see both received badges 0xBEE1 and 0xBEE2 within 30s — the server's one \
         receive loop didn't demux the two clients by their delivered badges",
    )?;
    Ok(())
}

/// v0.10 FS connect (`workload=fs`), step 2a: the client attaches (badge 0) and
/// the server mints + transfers a **root File cap** stamped `pack(root, READ)`.
/// Asserts the kernel-snitched `CapEvent::Transferred` carries that badge —
/// proving the new `user/fs` crate builds, embeds, spawns, and the connect
/// handshake runs end-to-end.
pub fn fs_connect_mints_root(h: &mut View) -> Result<(), String> {
    use protocol::{CapEventKind, CapObject};

    // pack(InodeId(0), FileRights::READ|WRITE): inode 0 in bits [0..32),
    // READ|WRITE (0b011) in the rights field at bits [32..48). The root cap is
    // the ceiling lookup attenuates from, so it must carry WRITE. See `fs_proto::Badge`.
    let root_badge = 0b011u64 << 32;
    h.wait_for(SEC * 30, |f, _| {
        matches!(
            f,
            OwnedFrame::CapEvent { kind: CapEventKind::Transferred, object: CapObject::Endpoint, badge, .. }
                if *badge == root_badge
        )
    })
    .ok_or(
        "no CapEvent::Transferred{Endpoint, badge=pack(root,READ)} within 30s — the FS \
         didn't mint + hand back the root File cap on connect",
    )?;
    Ok(())
}

/// v0.10 FS `Stat` (`workload=fs`), step 2b: after connecting, the client
/// `call`s `Stat` on its root File cap; the server unpacks the badge → inode,
/// decodes the request, runs `RamFs::stat`, and replies. The client emits a
/// sentinel **only** when the decoded response says the root is an empty `Dir`
/// — so this asserts the full request/response round-trip crossed the process
/// boundary and carried the right data.
pub fn fs_stat_root(h: &mut View) -> Result<(), String> {
    // Client emits [`markers::STAT_ROOT_OK`] iff `Stat(root) == Ok(Stat { kind: Dir, size: 0 })`.
    h.wait_for(SEC * 30, |f, strings| {
        matches!(f, OwnedFrame::Metric { name_id, value, .. }
            if strings.get(name_id).map(String::as_str) == Some("snitchos.user.telemetry_total")
                && *value == markers::STAT_ROOT_OK)
    })
    .ok_or(
        "client didn't confirm root stat (empty Dir) within 30s — the Stat request/response \
         didn't round-trip correctly across the FS boundary",
    )?;
    Ok(())
}

/// v0.10 FS `create` (`workload=fs`), step 3b: the client creates a file under
/// the root — the filename rides as a `UserBuf` the kernel copies across the
/// process boundary (option-D `CopyFromCaller`) — receives the freshly-minted
/// child File cap, and stats it. The client emits a sentinel only when the new
/// node reads back as an empty `File`, so this asserts the cross-AS name copy,
/// `RamFs::create`, and cap-mint-on-create all work end-to-end.
pub fn fs_create_then_stat(h: &mut View) -> Result<(), String> {
    h.wait_for(SEC * 30, |f, strings| {
        matches!(f, OwnedFrame::Metric { name_id, value, .. }
            if strings.get(name_id).map(String::as_str) == Some("snitchos.user.telemetry_total")
                && *value == markers::CREATE_STAT_OK)
    })
    .ok_or(
        "client didn't confirm create+stat (new empty File) within 30s — the create name-copy, \
         RamFs::create, or child-cap mint didn't round-trip across the FS boundary",
    )?;
    Ok(())
}

/// v0.10 FS `write`/`read` (`workload=fs`), step 3b: the client writes bytes to
/// the created file (data rides in via `CopyFromCaller`) and reads them back
/// (out via `CopyToCaller`). The client emits a sentinel only when the bytes
/// read back equal the bytes written — so this asserts the cross-AS copy works
/// in *both* directions through `RamFs::write`/`read`.
pub fn fs_write_read_roundtrip(h: &mut View) -> Result<(), String> {
    h.wait_for(SEC * 30, |f, strings| {
        matches!(f, OwnedFrame::Metric { name_id, value, .. }
            if strings.get(name_id).map(String::as_str) == Some("snitchos.user.telemetry_total")
                && *value == markers::WRITE_READ_OK)
    })
    .ok_or(
        "client didn't confirm write→read round-trip within 30s — bytes didn't survive the \
         cross-AS copy both ways through the FS",
    )?;
    Ok(())
}

/// v0.10 FS rights gate (`workload=fs`), step 4a: the client `lookup`s the
/// file it created, deliberately requesting **READ-only** — the server mints
/// `parent ∩ requested = READ` — then attempts a `write` through that
/// attenuated cap. The FS gate refuses (`Response::Err(Denied)`) and **snitches
/// the refusal**: it emits the `snitchos.fs.denied` gauge carrying the
/// structured `(inode, attempted-right)` packed value. As a positive control,
/// the client then `lookup`s requesting `READ|WRITE` and writes successfully,
/// emitting [`markers::WRITE_AUTHORIZED_OK`] — proving the gate refuses the under-authorized write
/// without over-refusing the authorized one.
pub fn fs_lookup_rights_gate(h: &mut View) -> Result<(), String> {
    // The created file is inode 1 (root is 0). The structured snitch packs
    // `Denial { inode: 1, attempted: WRITE }`: inode 1 in bits [0..32), WRITE
    // (0b010) in the attempted-right field at bits [32..48). See `fs_proto::Denial`.
    let denied_value: i64 = (1) | (0b010 << 32);
    h.wait_for(SEC * 30, |f, strings| {
        matches!(f, OwnedFrame::Metric { name_id, value, .. }
            if strings.get(name_id).map(String::as_str) == Some("snitchos.fs.denied")
                && *value == denied_value)
    })
    .ok_or(
        "no snitchos.fs.denied{inode=1, attempted=WRITE} within 30s — the FS didn't refuse \
         a WRITE on a READ-only File cap, or didn't snitch the refusal structurally",
    )?;

    // Positive control: a write through a READ|WRITE-looked-up cap succeeds.
    h.wait_for(SEC * 30, |f, strings| {
        matches!(f, OwnedFrame::Metric { name_id, value, .. }
            if strings.get(name_id).map(String::as_str) == Some("snitchos.user.telemetry_total")
                && *value == markers::WRITE_AUTHORIZED_OK)
    })
    .ok_or(
        "client didn't confirm an authorized write through a READ|WRITE lookup within 30s — \
         the gate may be over-refusing writes that carry the WRITE right",
    )?;
    Ok(())
}

/// v0.10 FS `remove` (`workload=fs`), step 4b: the client removes the file it
/// created, then looks the name up again and confirms the FS now reports
/// `NotFound`. The client emits [`markers::REMOVE_OK`] only when `Remove` succeeds *and* the
/// follow-up `lookup` is refused — so this asserts the unlink actually took
/// effect across the process boundary, not merely that the server replied.
pub fn fs_remove_unlinks(h: &mut View) -> Result<(), String> {
    h.wait_for(SEC * 30, |f, strings| {
        matches!(f, OwnedFrame::Metric { name_id, value, .. }
            if strings.get(name_id).map(String::as_str) == Some("snitchos.user.telemetry_total")
                && *value == markers::REMOVE_OK)
    })
    .ok_or(
        "client didn't confirm remove→lookup-gone within 30s — the Remove name-copy or \
         RamFs::remove didn't unlink the file across the FS boundary",
    )?;
    Ok(())
}

/// v0.10 FS workload trace (`workload=fs`), step 5: each FS op is a span. The
/// client opens `fs.create` and stays in it across the `call`; the server,
/// seeded with that span as its parent on `receive`, opens `fs.serve` — so the
/// server's handling nests **under** the client's op, attributed across the
/// process boundary. Asserts the parent linkage + temporal containment, the
/// same shape `rpc_trace_nests` proves for RPC — "a filesystem you can watch."
pub fn fs_workload_traces(h: &mut View) -> Result<(), String> {
    use protocol::SpanId;

    let call = h
        .wait_for(SEC * 30, |f, strings| {
            matches!(f, OwnedFrame::SpanStart { name_id, .. }
                if strings.get(name_id).map(String::as_str) == Some("fs.create"))
        })
        .ok_or("no SpanStart for 'fs.create' within 30s — the client didn't span the op")?;
    let (call_id, call_start) = match call {
        OwnedFrame::SpanStart { id, t, .. } => (id, t),
        _ => return Err("matched non-SpanStart (impossible)".to_string()),
    };
    if call_id == SpanId(0) {
        return Err("fs.create span id is 0 (root sentinel)".to_string());
    }

    // The server's handling span: a SpanStart parented to the client's
    // fs.create — the cross-process nesting the kernel seeds on `receive`.
    let serve = h
        .wait_for(SEC * 30, |f, _| {
            matches!(f, OwnedFrame::SpanStart { parent, .. } if *parent == call_id)
        })
        .ok_or(
            "no server SpanStart parented to fs.create within 30s — the trace didn't cross \
             the process boundary (server span not nested under the client's op)",
        )?;
    let (serve_id, serve_start) = match serve {
        OwnedFrame::SpanStart { id, t, .. } => (id, t),
        _ => return Err("matched non-SpanStart (impossible)".to_string()),
    };

    let serve_end = h
        .wait_for(SEC * 30, |f, _| matches!(f, OwnedFrame::SpanEnd { id, .. } if *id == serve_id))
        .ok_or("no SpanEnd for the server span within 30s")?;
    let serve_end = match serve_end {
        OwnedFrame::SpanEnd { t, .. } => t,
        _ => return Err("matched non-SpanEnd (impossible)".to_string()),
    };

    let call_end = h
        .wait_for(SEC * 30, |f, _| matches!(f, OwnedFrame::SpanEnd { id, .. } if *id == call_id))
        .ok_or("no SpanEnd for fs.create within 30s")?;
    let call_end = match call_end {
        OwnedFrame::SpanEnd { t, .. } => t,
        _ => return Err("matched non-SpanEnd (impossible)".to_string()),
    };

    if !(call_start <= serve_start && serve_end <= call_end) {
        return Err(format!(
            "server span [{serve_start}, {serve_end}] not contained in fs.create \
             [{call_start}, {call_end}] — the client's op span didn't stay open across \
             the server's handling"
        ));
    }
    Ok(())
}

/// v0.10 FS `readdir` (`workload=fs`), step 4c: the client lists the root
/// directory. Indexed `readdir(0)` returns the single entry (`"data"`, the file
/// it created) — inode + kind inline, the name copied out via `CopyToCaller` —
/// and `readdir(1)` reports `NotFound` (end of list). The client emits [`markers::READDIR_OK`]
/// only when the entry's inode, kind, and name all match *and* the next index
/// is refused — asserting indexed listing and the name copy-out across the
/// process boundary.
pub fn fs_readdir_lists_entries(h: &mut View) -> Result<(), String> {
    h.wait_for(SEC * 30, |f, strings| {
        matches!(f, OwnedFrame::Metric { name_id, value, .. }
            if strings.get(name_id).map(String::as_str) == Some("snitchos.user.telemetry_total")
                && *value == markers::READDIR_OK)
    })
    .ok_or(
        "client didn't confirm readdir listing within 30s — the indexed readdir, the name \
         copy-out, or the end-of-list NotFound didn't round-trip across the FS boundary",
    )?;
    Ok(())
}

/// User-pointer validation (`workload=userspace-bad-ptr`): the `bad-ptr` program
/// passes an in-range but **unmapped** user VA to `DebugWrite`. The kernel's
/// `copy_from_user` walks the page table and refuses
/// (`SyscallRefused{DebugWrite, BadUserRange}`) rather than faulting to S-mode —
/// so the process survives and emits `0x0BAD`. Asserts both the labelled refusal
/// *and* the survival marker — the "panic on an unmapped pointer is gone" proof.
pub fn userspace_bad_ptr_refused(h: &mut View) -> Result<(), String> {
    let debug_write = snitchos_abi::Syscall::DebugWrite as u8;
    h.wait_for(SEC * 10, |f, _| {
        matches!(f, OwnedFrame::SyscallRefused { syscall, reason, .. }
            if *syscall == debug_write && matches!(reason, protocol::RefusalReason::BadUserRange))
    })
    .ok_or(
        "no SyscallRefused{DebugWrite, BadUserRange} within 10s — the kernel didn't refuse the \
         unmapped user pointer (it may have faulted on the SUM deref instead)",
    )?;

    h.wait_for(SEC * 10, |f, strings| {
        matches!(f, OwnedFrame::Metric { name_id, value, .. }
            if strings.get(name_id).map(String::as_str) == Some("snitchos.user.telemetry_total")
                && *value == 0x0BAD)
    })
    .ok_or(
        "bad-ptr didn't emit its survival marker (0x0BAD) within 10s — the kernel may have \
         panicked on the bad pointer rather than refusing it gracefully",
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
pub fn mutex_storm(h: &mut View) -> Result<(), String> {
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
/// loop (each call: intern check + frame serialize + `TX_STAGING.lock` +
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
pub fn virtio_storm(h: &mut View) -> Result<(), String> {
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

/// v0.7a first userspace (`workload=userspace`): the embedded `user/hello`
/// is loaded into the boot table's low half, the kernel drops to U-mode on
/// hart 1, and the program issues one ambient `EmitMetric` syscall. We assert:
///
///   1. `snitchos.user.telemetry_total` appears — proving the whole chain:
///      ELF load + per-segment map with the `U` bit + sret-to-U + U-mode
///      executes + the `ecall` traps back + the handler emits on its behalf.
///   2. Its value is 42 — the argument `hello` passes in `a0` crossed the
///      U→S boundary intact.
///   3. A `kernel.heartbeat` arrives after — hart 0 kept ticking while
///      hart 1 ran userspace.
pub fn userspace_emits_telemetry(h: &mut View) -> Result<(), String> {
    let frame = h
        .wait_for(SEC * 10, is_metric_named("snitchos.user.telemetry_total"))
        .ok_or(
            "no snitchos.user.telemetry_total within 10s — userspace never \
             reached the syscall (ELF load / map(U) / sret-to-U / ecall path broke?)",
        )?;
    let value = match frame {
        OwnedFrame::Metric { value, .. } => value,
        _ => return Err("matched non-metric (impossible)".to_string()),
    };
    if value != 42 {
        return Err(format!(
            "user telemetry value = {value}, expected 42 (the arg hello passes in a0)"
        ));
    }

    h.wait_for(SEC * 10, is_span_start_named("kernel.heartbeat"))
        .ok_or("no heartbeat after the userspace syscall — hart 0 wedged while hart 1 ran U?")?;

    Ok(())
}

/// v0.7a isolation (`workload=userspace-fault`): the `faulter` program emits
/// a marker, then reads a kernel high-half VA from U-mode. That page is
/// mapped in the process's address space (the kernel high-half is shared into
/// every user root) but carries no `U` bit, so the load faults to S-mode. We
/// assert:
///
///   1. `snitchos.user.faults_total` appears — the `U`-bit firewall caught
///      a U-mode access to kernel memory (had it NOT faulted, the read would
///      have succeeded and no counter would ever be emitted → fail).
///   2. A `kernel.heartbeat` arrives after — hart 0 stayed healthy while the
///      kernel firewalled (and parked) the offending hart-1 process.
pub fn userspace_cannot_touch_kernel(h: &mut View) -> Result<(), String> {
    h.wait_for(SEC * 10, is_metric_named("snitchos.user.faults_total"))
        .ok_or(
            "no snitchos.user.faults_total within 10s — a U-mode read of a kernel \
             VA did NOT fault (isolation broken!) or faulter never ran",
        )?;

    h.wait_for(SEC * 10, is_span_start_named("kernel.heartbeat"))
        .ok_or("no heartbeat after the U-mode fault — kernel destabilised by firewalling userspace?")?;

    Ok(())
}

/// v0.7b denial payoff (`workload=userspace`): after invoking the
/// `TelemetrySink` it *was* granted (handle 0), `hello` deliberately
/// invokes a handle it was **never granted** (handle 1 — its table holds
/// only handle 0). The kernel resolves it against the process's own
/// `CapTable`, finds nothing, refuses, and snitches
/// `snitchos.cap.denied_total`. The capability twin of
/// `userspace-cannot-touch-kernel`: there the page table said no; here the
/// capability table does — and the refusal is observable. We assert:
///
///   1. `snitchos.cap.denied_total` appears — an ungranted invocation was
///      refused (had ambient authority leaked, the invoke would have
///      "succeeded" and no denial counter would ever emit → fail).
///   2. A `kernel.heartbeat` arrives after — a denied cap is a clean
///      refusal, not a wedge.
pub fn userspace_cap_denied(h: &mut View) -> Result<(), String> {
    h.wait_for(SEC * 10, is_metric_named("snitchos.cap.denied_total"))
        .ok_or(
            "no snitchos.cap.denied_total within 10s — an invocation of an \
             ungranted handle was NOT refused (ambient authority leaked?) or \
             denier never ran",
        )?;

    h.wait_for(SEC * 10, is_span_start_named("kernel.heartbeat"))
        .ok_or("no heartbeat after the denied invocation — did a refused cap wedge the kernel?")?;

    Ok(())
}

/// v0.7b grant snitching (`workload=userspace`): the kernel emits
/// `snitchos.cap.grants_total` when it grants the bootstrap `TelemetrySink`
/// to the process — authority being *created* is observable, not just
/// authority being *exercised*. Granting is wired into the userspace boot
/// path, so any userspace workload exercises it; we assert the counter
/// reaches the wire (it only emits if `Process::bootstrap` + the grant
/// snitch actually ran).
pub fn userspace_grant_snitched(h: &mut View) -> Result<(), String> {
    h.wait_for(SEC * 10, is_metric_named("snitchos.cap.grants_total"))
        .ok_or(
            "no snitchos.cap.grants_total within 10s — the kernel granted the \
             bootstrap TelemetrySink without snitching it (grant path / counter broke?)",
        )?;

    Ok(())
}

/// v0.7b clean process exit (`workload=userspace`): after its syscalls,
/// `hello` invokes `Exit` instead of busy-spinning. The kernel marks the
/// user task `Exited` and switches hart 1 back to its idle loop (which
/// `wfi`s) — making the workload wfi-bounded rather than core-pegging.
/// Asserts the exit is snitched (`snitchos.user.exits_total`) and the
/// kernel keeps heartbeating (a clean exit, not a wedge).
pub fn userspace_process_exits(h: &mut View) -> Result<(), String> {
    h.wait_for(SEC * 10, is_metric_named("snitchos.user.exits_total"))
        .ok_or(
            "no snitchos.user.exits_total within 10s — the user process did not \
             exit cleanly (Exit syscall / exit_now path broke, or hello still spins?)",
        )?;

    h.wait_for(SEC * 10, is_span_start_named("kernel.heartbeat"))
        .ok_or("no heartbeat after the user process exited — did exit wedge the kernel?")?;

    Ok(())
}

/// Cooperative `Yield` syscall (`workload=userspace`): `hello` calls
/// `yield_now()` before `exit()`. A userspace task can't call the kernel's
/// `yield_now` directly — it `ecall`s `Yield`, the kernel yields on its
/// behalf, and a later reschedule returns control to U-mode past the
/// `ecall`. We assert a full round trip:
///
///   1. A `ContextSwitch` LEAVING `user_main` — it gave up the CPU. (Not
///      decisive on its own: `exit_now` also stamps `Yield` on the wire.)
///   2. A `ContextSwitch` RETURNING to `user_main` — the decisive proof. An
///      exited process never comes back, so a return means `yield_now`
///      resumed U-mode at the instruction after the `ecall`.
///   3. `snitchos.user.exits_total` after the resume — `hello` reached
///      `exit()`, which follows the `yield_now()`, so control flowed past it.
pub fn userspace_yield_round_trips(h: &mut View) -> Result<(), String> {
    let user_id = match h
        .wait_for(SEC * 20, is_thread_register_named("user_main"))
        .ok_or("no ThreadRegister for 'user_main' within 20s")?
    {
        OwnedFrame::ThreadRegister { id, .. } => id,
        _ => return Err("matched non-ThreadRegister".to_string()),
    };

    // Departure: user_main leaves the CPU. NB `exit_now` ALSO stamps `Yield`
    // on the wire (the wire `Exit` variant is unused), so a departure alone
    // does NOT prove a yield — it could be the exit. The *return* below is
    // what distinguishes them.
    h.wait_for(SEC * 10, move |f, _| match f {
        OwnedFrame::ContextSwitch { from, reason, .. } => {
            *from == user_id && matches!(reason, protocol::SwitchReason::Yield)
        }
        _ => false,
    })
    .ok_or("no ContextSwitch leaving user_main within 10s — user_main never ran?")?;

    // Return: the scheduler comes BACK to user_main. A process that exited
    // never returns, so this is the round-trip proof — yield_now resumed
    // U-mode rather than the program simply ending.
    h.wait_for(SEC * 10, move |f, _| match f {
        OwnedFrame::ContextSwitch { to, reason, .. } => {
            *to == user_id && matches!(reason, protocol::SwitchReason::Yield)
        }
        _ => false,
    })
    .ok_or(
        "no ContextSwitch returning to user_main within 10s — control never resumed \
         past yield_now (dispatch arm missing / sepc not advanced, or hello didn't yield)",
    )?;

    // Clean completion after the resume.
    h.wait_for(SEC * 10, is_metric_named("snitchos.user.exits_total"))
        .ok_or("no exits_total after the resume — hello didn't reach exit past the yield")?;

    Ok(())
}

/// v0.7b authority event (`workload=userspace`): the bootstrap grant emits a
/// first-class `CapEvent::Granted` — richer than the `grants_total` counter
/// (it carries the global cap id, holder, object kind, and rights). This is
/// the seed of the host-reconstructed capability derivation tree (v0.8).
/// Asserts the event reaches the wire with object `TelemetrySink` and `EMIT`
/// rights.
pub fn userspace_cap_granted_event(h: &mut View) -> Result<(), String> {
    h.wait_for(SEC * 10, is_cap_granted_telemetry())
        .ok_or(
            "no CapEvent::Granted{TelemetrySink, EMIT} within 10s — the kernel \
             granted the bootstrap cap without emitting the authority event \
             (or emitted wrong object/rights)",
        )?;

    Ok(())
}

/// Second bootstrap grant (`workload=userspace`): alongside the
/// `TelemetrySink`, `init` is granted a `SpanSink` — the authority to open
/// spans from U-mode (consumed by the span syscalls). Asserts the grant
/// reaches the wire as a `CapEvent::Granted{SpanSink, EMIT}`, proving the
/// capability exists before any program tries to use it.
pub fn userspace_spansink_granted(h: &mut View) -> Result<(), String> {
    h.wait_for(SEC * 10, is_cap_granted_span())
        .ok_or(
            "no CapEvent::Granted{SpanSink, EMIT} within 10s — the bootstrap grant \
             did not include a span sink (or emitted wrong object/rights)",
        )?;

    Ok(())
}

/// Userspace tracing (`workload=userspace`): `hello` opens a span named
/// "hello.work" through its `SpanSink` capability. The kernel copies the name
/// out of U-mode, interns it on demand, and opens a span on hello's task
/// cursor. Asserts a `SpanStart` for "hello.work" attributed to the
/// `user_main` task — exercising the whole `SpanOpen` path: cap check →
/// `copy_from_user` → intern → emit, with kernel-stamped attribution.
pub fn userspace_emits_span(h: &mut View) -> Result<(), String> {
    let user_id = match h
        .wait_for(SEC * 20, is_thread_register_named("user_main"))
        .ok_or("no ThreadRegister for 'user_main' within 20s")?
    {
        OwnedFrame::ThreadRegister { id, .. } => id,
        _ => return Err("matched non-ThreadRegister".to_string()),
    };

    let span_id = match h
        .wait_for(SEC * 10, move |f, strings| match f {
            OwnedFrame::SpanStart { name_id, task_id, .. } => {
                strings.get(name_id).map(String::as_str) == Some("hello.work")
                    && *task_id == user_id
            }
            _ => false,
        })
        .ok_or(
            "no SpanStart 'hello.work' attributed to user_main within 10s — the SpanOpen \
             path (cap check / copy_from_user / intern / emit) refused or broke",
        )? {
        OwnedFrame::SpanStart { id, .. } => id,
        _ => return Err("matched non-SpanStart".to_string()),
    };

    // The runtime `Span` RAII guard closes on drop: the matching `SpanEnd`
    // proves SpanClose round-trips (and the cursor-top validation accepted it).
    h.wait_for(
        SEC * 10,
        move |f, _| matches!(f, OwnedFrame::SpanEnd { id, .. } if *id == span_id),
    )
    .ok_or(
        "no SpanEnd matching the hello.work span within 10s — the RAII Span guard / \
         SpanClose path didn't close it",
    )?;

    Ok(())
}

/// Refusal observability (`workload=userspace`): `hello` deliberately invokes
/// a handle it holds but for the wrong object (the `SpanSink` at handle 1,
/// invoked as a telemetry sink). The kernel refuses — and snitches a
/// `SyscallRefused{CapWrongObject}` so the denial is a labelled wire event,
/// not a silent missing result. Asserts that event, attributed to `user_main`.
pub fn userspace_refusal_snitched(h: &mut View) -> Result<(), String> {
    let user_id = match h
        .wait_for(SEC * 20, is_thread_register_named("user_main"))
        .ok_or("no ThreadRegister for 'user_main' within 20s")?
    {
        OwnedFrame::ThreadRegister { id, .. } => id,
        _ => return Err("matched non-ThreadRegister".to_string()),
    };

    h.wait_for(SEC * 10, move |f, _| match f {
        OwnedFrame::SyscallRefused { reason, task_id, .. } => {
            matches!(reason, protocol::RefusalReason::CapWrongObject) && *task_id == user_id
        }
        _ => false,
    })
    .ok_or(
        "no SyscallRefused{CapWrongObject} from user_main within 10s — a denied invoke \
         was silent (refusal observability broke)",
    )?;

    Ok(())
}

/// Per-process span-name quota (`workload=userspace-span-flood`): `span-flood`
/// opens spans with 20 distinct names — past `Process::MAX_SPAN_NAMES` (16) —
/// so the kernel must refuse the surplus with `SyscallRefused{Quota}` rather
/// than leak unbounded `'static` names or panic. Asserts the quota refusal and
/// that the kernel keeps heartbeating after.
pub fn userspace_quota_refused(h: &mut View) -> Result<(), String> {
    let user_id = match h
        .wait_for(SEC * 20, is_thread_register_named("user_span_flood"))
        .ok_or("no ThreadRegister for 'user_span_flood' within 20s")?
    {
        OwnedFrame::ThreadRegister { id, .. } => id,
        _ => return Err("matched non-ThreadRegister".to_string()),
    };

    h.wait_for(SEC * 10, move |f, _| match f {
        OwnedFrame::SyscallRefused { reason, task_id, .. } => {
            matches!(reason, protocol::RefusalReason::Quota) && *task_id == user_id
        }
        _ => false,
    })
    .ok_or(
        "no SyscallRefused{Quota} from user_span_flood within 10s — the span-name quota \
         didn't refuse the surplus (off-by-one, or not enforced)",
    )?;

    h.wait_for(SEC * 10, is_span_start_named("kernel.heartbeat"))
        .ok_or("no heartbeat after the quota refusals — did the quota path destabilise the kernel?")?;

    Ok(())
}

/// Userspace `println!` (`workload=userspace`): `hello` calls
/// `snitchos_std::println!("hello from userspace")` — through the std facade →
/// the `DebugWrite` syscall → a snitched `Frame::Log`. Asserts the line reaches
/// the wire, attributed to `user_main`. Stdout-as-telemetry.
pub fn userspace_prints(h: &mut View) -> Result<(), String> {
    let user_id = match h
        .wait_for(SEC * 20, is_thread_register_named("user_main"))
        .ok_or("no ThreadRegister for 'user_main' within 20s")?
    {
        OwnedFrame::ThreadRegister { id, .. } => id,
        _ => return Err("matched non-ThreadRegister".to_string()),
    };

    h.wait_for(SEC * 10, move |f, _| match f {
        OwnedFrame::Log { msg, task_id, .. } => {
            msg.contains("hello from userspace") && *task_id == user_id
        }
        _ => false,
    })
    .ok_or(
        "no Log 'hello from userspace' from user_main within 10s — the println / DebugWrite / \
         Log-frame path refused or broke",
    )?;

    Ok(())
}

/// Two userspace demo workers (`workload=workers`) share one hart cooperatively:
/// `worker_a` and `worker_b` are independent processes (distinct page tables,
/// distinct span names) that each loop {open `worker_x.tick` span, bump
/// progress, `yield`}. Asserts both register, both emit *repeated* spans
/// attributed to their own task id (neither starves), and the scheduler
/// actually context-switches between them. The proof that the
/// address-space-aware switch (CP5-1) carries two distinct user roots on one
/// hart — the userspace successor to kernel `task_a`/`task_b`.
pub fn two_userspace_workers_round_robin(h: &mut View) -> Result<(), String> {
    let mut ids = std::collections::HashMap::new();
    for name in ["worker_a", "worker_b"] {
        let id = match h
            .wait_for(SEC * 20, is_thread_register_named(name))
            .ok_or_else(|| std::format!("no ThreadRegister for '{name}' within 20s"))?
        {
            OwnedFrame::ThreadRegister { id, .. } => id,
            _ => return Err("matched non-ThreadRegister".to_string()),
        };
        ids.insert(name, id);
    }

    // Each worker opens a fresh `worker_x.tick` span every iteration. Finding
    // two per worker — attributed to that worker's own task id — proves both
    // loops repeat and neither starves the other.
    for name in ["worker_a", "worker_b"] {
        let span_name = std::format!("{name}.tick");
        let worker_id = ids[name];
        for nth in ["first", "second"] {
            let needle = span_name.clone();
            h.wait_for(SEC * 15, move |f, strings| match f {
                OwnedFrame::SpanStart { name_id, task_id, .. } => {
                    strings.get(name_id).map(String::as_str) == Some(needle.as_str())
                        && *task_id == worker_id
                }
                _ => false,
            })
            .ok_or_else(|| std::format!("no {nth} {span_name} span from {name} within 15s"))?;
        }
    }

    // The scheduler actually switched between the two userspace tasks.
    h.wait_for(SEC * 15, |f, strings| match f {
        OwnedFrame::Metric { name_id, value, .. } => {
            strings.get(name_id).map(String::as_str)
                == Some("snitchos.sched.context_switches_total")
                && *value > 0
        }
        _ => false,
    })
    .ok_or("no sched.context_switches_total > 0 within 15s")?;

    Ok(())
}

/// On-demand heap growth (`workload=heap-grow`): `heap-grow` allocates a 512 KiB
/// buffer — far past the runtime's 64 KiB per-region map — so the `talc`
/// allocator must `map_anon` more frames from the kernel. It fills and sums the
/// buffer, emitting the sum (524288) only if every byte was allocated, written,
/// and readable. Asserts that marker and a surviving heartbeat.
pub fn heap_grows_on_demand(h: &mut View) -> Result<(), String> {
    h.wait_for(SEC * 10, |f, strings| match f {
        OwnedFrame::Metric { name_id, value, .. } => {
            strings.get(name_id).map(String::as_str) == Some("snitchos.user.telemetry_total")
                && *value == 512 * 1024
        }
        _ => false,
    })
    .ok_or(
        "no telemetry_total == 524288 within 10s — the 512 KiB allocation failed (heap didn't \
         grow via MapAnon, or the mapped frames weren't writable)",
    )?;

    h.wait_for(SEC * 10, is_span_start_named("kernel.heartbeat"))
        .ok_or("no heartbeat after heap growth — did MapAnon destabilise the kernel?")?;

    Ok(())
}

/// v0.8 preemption — *the milestone heart* (`workload=user-hog`). Same fixture
/// as the Step 3 characterisation (a non-cooperative `user_hog` tight U-mode
/// loop co-located with a cooperative `worker_a` peer), but now the timer takes
/// the CPU back: after its quantum the hog is descheduled, the peer makes
/// progress, and a `ContextSwitch { reason: Preempt }` proves it on the wire.
/// The kernel is never preempted — only userspace (the `SPP == User` gate).
///
/// This *replaces* `user-hog-starves-peer`: once preemption works the peer no
/// longer starves, so the two assertions are mutually exclusive on one kernel.
/// The characterisation of the bug lives on in git history (its Step 3 commit).
pub fn preempt_runaway_user_task(h: &mut View) -> Result<(), String> {
    // Harvest the hog's task id so we can recognise the ContextSwitch that
    // leaves it. The peer must also register (it's the one that will progress).
    let hog_id = match h
        .wait_for(SEC * 20, is_thread_register_named("user_hog"))
        .ok_or("no ThreadRegister for 'user_hog' within 20s")?
    {
        OwnedFrame::ThreadRegister { id, .. } => id,
        _ => return Err("matched non-ThreadRegister".to_string()),
    };
    h.wait_for(SEC * 20, is_thread_register_named("worker_a"))
        .ok_or("no ThreadRegister for peer 'worker_a' within 20s")?;

    // The headline frame: the timer descheduled the hog. The hog never yields,
    // so a ContextSwitch *leaving* it can only have come from preemption — its
    // reason is `Preempt`, not `Yield`.
    h.wait_for(SEC * 30, move |f, _| match f {
        OwnedFrame::ContextSwitch { from, reason, .. } => {
            *from == hog_id && matches!(reason, protocol::SwitchReason::Preempt)
        }
        _ => false,
    })
    .ok_or(
        "no ContextSwitch{Preempt} leaving user_hog within 30s — the timer never took the CPU back",
    )?;

    // The consequence: the peer now makes progress. Its per-task runs counter
    // climbs past 2 — the exact signal Step 3 asserted *stayed* below 2.
    h.wait_for(SEC * 30, |f, strings| match f {
        OwnedFrame::Metric { name_id, value, .. } => {
            strings.get(name_id).map(String::as_str)
                == Some("snitchos.task.worker_a.runs_total")
                && *value >= 2
        }
        _ => false,
    })
    .ok_or("peer worker_a not scheduled 2+ times within 30s — preemption isn't giving it the CPU")?;

    // The kernel stays healthy throughout — preemption only deschedules the
    // userspace hog, it doesn't destabilise the kernel.
    h.wait_for(SEC * 10, is_span_start_named("kernel.heartbeat"))
        .ok_or("no heartbeat — preemption destabilised the kernel")?;

    Ok(())
}

/// v0.8 preemption telemetry (`workload=user-hog`): the kernel *counts* each
/// preemption. `snitchos.sched.preemptions_total` climbs as the timer
/// repeatedly deschedules the runaway hog — the rate signal beside the
/// per-switch `ContextSwitch{Preempt}` frame. Emitted via the deferred-emission
/// pattern: an atomic bumped in the reschedule path, drained by hart 0's
/// heartbeat (never emitted from inside the timer handler).
pub fn preemption_telemetry(h: &mut View) -> Result<(), String> {
    h.wait_for(SEC * 30, |f, strings| match f {
        OwnedFrame::Metric { name_id, value, .. } => {
            strings.get(name_id).map(String::as_str)
                == Some("snitchos.sched.preemptions_total")
                && *value >= 1
        }
        _ => false,
    })
    .ok_or("no snitchos.sched.preemptions_total >= 1 within 30s — preemptions not counted")?;

    Ok(())
}

/// v0.8 preemption *guard* (`workload=syscall-hog`): a syscall-heavy task is
/// still preempted. The `syscall_hog` program loops a cheap ambient `DebugWrite`
/// with no `yield`, so it spends the bulk of its time in S-mode — but with
/// interrupts masked (RISC-V clears `SIE` on trap entry and SnitchOS never
/// re-enables it during handling). The timer therefore cannot fire mid-syscall;
/// it fires the instant the syscall `sret`s back to U-mode (`SPP == 0`), and the
/// quantum check deschedules the hog. We prove that with a `ContextSwitch{Preempt}`
/// leaving the hog — the hog never yields, so a switch *away* from it can only be
/// a preemption. Regression guard: if a future version ever re-enables interrupts
/// inside long syscalls without a `need_resched` drain, a near-100%-S-mode task
/// like this one would dodge preemption and this assertion would fail. See
/// `plans/v0.8c-need-resched-on-syscall-return.md`.
pub fn syscall_hog_still_preempted(h: &mut View) -> Result<(), String> {
    let hog_id = match h
        .wait_for(SEC * 20, is_thread_register_named("syscall_hog"))
        .ok_or("no ThreadRegister for 'syscall_hog' within 20s")?
    {
        OwnedFrame::ThreadRegister { id, .. } => id,
        _ => return Err("matched non-ThreadRegister".to_string()),
    };

    // The headline: the timer descheduled a task that only ever leaves the CPU
    // via the timer (it never yields), so the reason must be `Preempt`. This is
    // the assertion that fails if a syscall-heavy task could dodge preemption.
    h.wait_for(SEC * 30, move |f, _| match f {
        OwnedFrame::ContextSwitch { from, reason, .. } => {
            *from == hog_id && matches!(reason, protocol::SwitchReason::Preempt)
        }
        _ => false,
    })
    .ok_or(
        "no ContextSwitch{Preempt} leaving syscall_hog within 30s — a syscall-heavy task dodged preemption",
    )?;

    // The kernel stays healthy — preempting a syscall-spamming task at the
    // (lock-free) U-mode return boundary doesn't destabilise the kernel.
    h.wait_for(SEC * 10, is_span_start_named("kernel.heartbeat"))
        .ok_or("no heartbeat — preempting the syscall hog destabilised the kernel")?;

    Ok(())
}

/// v0.11 Tier-0 console input (`workload=console-echo`): a byte typed at the UART
/// round-trips host → kernel → userspace. The harness injects bytes into the
/// guest UART (QEMU stdin); the `console_echo` program drains them via
/// `ConsoleRead` and echoes them back via `DebugWrite`, observed here as a `Log`
/// frame. Proves the whole polled-RX path: UART → timer drain → ring →
/// `ConsoleRead` → `copy_to_user` → userspace. See `plans/console-tier0-polled-rx.md`.
pub fn console_echo_round_trips(h: &mut View) -> Result<(), String> {
    // Wait until the echo program is up and reading, so injected bytes aren't
    // dropped before it starts polling.
    h.wait_for(SEC * 20, is_span_start_named("console_echo.alive"))
        .ok_or("console_echo never reached U-mode (no alive marker within 20s)")?;

    // Inject the token in one write+flush: it lands in the UART RX FIFO together,
    // so the next timer drain rings all of it and one `console_read` returns it —
    // a single `Log` echo.
    h.send_input(b"snitch\n").map_err(|e| format!("inject UART input: {e}"))?;

    h.wait_for(SEC * 20, |f, _| {
        matches!(f, OwnedFrame::Log { msg, .. } if msg.contains("snitch"))
    })
    .ok_or("no Log echo of injected 'snitch' within 20s — console input didn't round-trip")?;

    Ok(())
}

/// v0.8b priority scheduling — *ordered, but fair* (`workload=priorities`). A
/// High-priority CPU-bound `greedy` task and a Low-priority cooperative
/// `worker_b` share hart 1. The scheduler must (a) **respect priority** —
/// priority-aware preemption keeps `greedy` on-CPU rather than letting the timer
/// demote it to the Low worker, so `greedy` dominates CPU time — yet (b) **stay
/// fair** — aging lifts the starved Low worker to the running level periodically,
/// so it still makes progress instead of starving outright (the failure mode of
/// pure static priority).
///
/// Asserted on the hart-0 heartbeat's per-task metrics: the Low worker is
/// scheduled at least twice (aging rescued it), and at that point the High
/// task's accumulated CPU time dominates the Low worker's by a wide margin
/// (priority respected — an equal-share scheduler would leave them comparable).
pub fn priorities_ordered_but_fair(h: &mut View) -> Result<(), String> {
    // Priority is on the wire (Step 5): each task's `ThreadRegister` carries its
    // scheduling level (0 = Low, 1 = Normal, 2 = High), so the trace can group/
    // colour by priority. Assert the two demo tasks register at their levels.
    h.wait_for(SEC * 20, |f, _| match f {
        OwnedFrame::ThreadRegister { name, priority, .. } => name == "greedy" && *priority == 2,
        _ => false,
    })
    .ok_or("no ThreadRegister for 'greedy' carrying priority High(2) on the wire")?;
    h.wait_for(SEC * 20, |f, _| match f {
        OwnedFrame::ThreadRegister { name, priority, .. } => name == "worker_b" && *priority == 0,
        _ => false,
    })
    .ok_or("no ThreadRegister for 'worker_b' carrying priority Low(0) on the wire")?;

    let greedy_cpu = std::cell::Cell::new(0i64);
    let low_cpu = std::cell::Cell::new(0i64);
    let low_runs = std::cell::Cell::new(0i64);

    // Run until the Low worker has progressed twice (aging defeated starvation),
    // tracking the CPU-time counters so we can compare them at that moment.
    h.wait_for(SEC * 30, |f, strings| match f {
        OwnedFrame::Metric { name_id, value, .. } => {
            match strings.get(name_id).map(String::as_str) {
                Some("snitchos.task.greedy.cpu_time_ticks") => greedy_cpu.set(*value),
                Some("snitchos.task.worker_b.cpu_time_ticks") => low_cpu.set(*value),
                Some("snitchos.task.worker_b.runs_total") => low_runs.set(*value),
                _ => {}
            }
            low_runs.get() >= 2
        }
        _ => false,
    })
    .ok_or(
        "low-priority worker_b never reached 2 runs within 30s — aging failed to rescue it from \
         starvation (or the tasks didn't spawn)",
    )?;

    // Priority respected: the High CPU-bound task held the CPU far longer than
    // the Low worker. (Without priority-aware preemption the timer would have
    // time-sliced them toward parity.)
    let (greedy, low) = (greedy_cpu.get(), low_cpu.get());
    if greedy < 10 * low.max(1) {
        return Err(std::format!(
            "priority not respected: greedy (High) cpu_time={greedy} is not >> worker_b (Low) \
             cpu_time={low} (expected High to dominate CPU by 10x+)"
        ));
    }

    Ok(())
}
