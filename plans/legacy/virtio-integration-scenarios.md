# Plan: Virtio integration scenarios

**Status**: Active

## Goal

Add two integration scenarios that verify the virtio TX ring handles multiple cycles without losing frames, and that every span closes cleanly.

## Background

The TX virtqueue has 8 descriptor slots. Each heartbeat tick emits ~6 frames
(SpanStart + 4 Metrics + SpanEnd). After the second heartbeat the ring must
wrap; after five it has wrapped several times. The existing scenarios prove the
kernel boots and the timer fires, but none of them verify:

- frames transmitted without loss through a wrapped ring
- SpanEnd frames arrive and match their SpanStart

## New matchers needed

Both scenarios need matchers that don't exist yet:

- `is_metric_named(name)` — matches a `Metric` frame by resolving its `name_id`
  through the string table
- `is_span_end_for(span_id)` — matches a `SpanEnd` with a specific `SpanId`

## Steps

### Step 1 — `ring-wraps-without-drop`

**What it tests**: the TX ring can wrap without silently losing frames.

**Observable**: `heartbeat.count` is emitted as a counter metric each tick,
starting at 1 and incrementing by exactly 1. Three consecutive values must be
sequential integers — any gap means a frame was lost mid-ring.

**Acceptance criteria**:
- `cargo xtask test ring-wraps-without-drop` passes
- Test captures three `heartbeat.count` metric values
- Values are sequential (V2 == V1 + 1, V3 == V2 + 1)
- Test fails (and says why) if values are non-sequential or frames time out

**New infrastructure**: add `is_metric_named(name)` to `matchers.rs`.

**RED**: write the scenario body — it calls `wait_for(is_metric_named("heartbeat.count"))`
three times, pattern-matches the returned `OwnedFrame::Metric { value, .. }` to
extract each value, and asserts sequentiality. Won't compile until the matcher
exists.

**GREEN**: add `is_metric_named` to `matchers.rs`; register the scenario in
`itest.rs`.

**Done when**: `cargo xtask test ring-wraps-without-drop` passes.

---

### Step 2 — `spans-close-cleanly`

**What it tests**: SpanEnd frames arrive for every heartbeat span, with the
correct SpanId.

**Observable**: for two consecutive heartbeat spans, capture the SpanId from the
SpanStart, then assert a matching SpanEnd arrives before the next tick begins.

**Acceptance criteria**:
- `cargo xtask test spans-close-cleanly` passes
- Captures SpanId from two heartbeat SpanStart frames
- Asserts a matching SpanEnd arrives for each
- Test fails (and says why) if any SpanEnd is missing or has the wrong id

**New infrastructure**: add `is_span_end_for(span_id: SpanId)` to `matchers.rs`.

**RED**: write the scenario — calls `wait_for(is_span_start_named("kernel.heartbeat"))`,
extracts the `SpanId`, then calls `wait_for(is_span_end_for(id))`. Won't compile
until the matcher exists.

**GREEN**: add `is_span_end_for` to `matchers.rs`; register the scenario.

**Done when**: `cargo xtask test spans-close-cleanly` passes.

---

## Pre-PR quality gate

- `cargo xtask test` (all scenarios pass)
- `cargo build -p xtask` (clean compile)
- Mutation testing not applicable (integration tests; no unit-testable logic added)

---
*Delete when complete. If `plans/` is empty, delete the directory.*
