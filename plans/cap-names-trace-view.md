# Plan: Cap names in the trace view

**Branch**: main (project rule: all work lands on `main`; user handles commits)
**Status**: Active

## Goal

The collector reconstructs the **named capability derivation tree** from `CapEvent`
frames into OTLP spans, so Tempo shows the grant graph — every cap a hold-duration
bar (grant → revoke), edges by `parent_cap_id`, each node named — in its own
`capabilities` trace.

## Context / where this sits

The names are already **on the wire**: `Frame::CapEvent` carries `cap_id`,
`parent_cap_id`, `holder`, `object`, `rights`, `badge`, `t`, and a NUL-padded
`name`. The static reconstruction is already **done offline**:
`diagram/src/caps.rs::derivation_tree(frames) -> Graph` folds those frames into a
Mermaid tree for `docs/generated/caps.md`. What's missing is the **live Tempo
view**: `collector/src/state.rs` currently handles `Frame::CapEvent` by only
`advance_anchor(t)` — the authority event passes without being drawn. This plan
closes that gap.

## Settled design decisions

- **Span model — hold-duration.** Each `cap_id` is ONE span. `Granted`/`Transferred`
  starts it (start = grant `t`); `Revoked` ends it (end = revoke `t`); a still-held
  cap is closed at **flush** with a synthetic end = last-seen `t`. Rationale: a
  capability *is* a holding with a lifetime; a transitive revoke closes a whole
  subtree of bars at one right edge — the "reclaim" story reads for free. The one
  honest cost: OTLP can't stream an *unclosed* span, so a never-revoked cap only
  materialises at flush — fine for this project's session/itest + after-the-fact
  Grafana workflow.
- **Separate `capabilities` trace_id.** Keeps the authority graph isolated from the
  session's task/heartbeat spans and sidesteps `cap_id` vs kernel `SpanId`
  span-id collisions.
- **Reuse the diagram's modeling conventions** (already host-tested there):
  - Drop one-shot `CapObject::Reply` caps as derivation noise.
  - `parent_cap_id == 0` = genuinely-root grant (no OTLP parent).
  - Label = object `name` if non-empty, else the object-kind name; carry `holder`.
  - A revoked cap is tagged (span attribute), distinct from still-held.
- **Attributes** on each cap span: `cap.holder`, `cap.rights`, `cap.object`,
  `cap.badge`, `cap.revoked` (bool: reclaimed vs held-at-flush). Name → the span
  name itself.
- **Span events on the bar.** Each cap span carries OTLP `events` — timestamped
  annotations marking the moments *on* the lifetime: `granted@t`, `transferred@t`,
  `revoked@t` (with the moving `holder` as an event attribute). This puts the
  timeline inside the structure natively (no duplicate spans). It renders at span
  close/flush like everything span-side — it is **not** the live "watch it grow"
  signal (that's the metric/log follow-up below; OTLP has no open-span update).

## Acceptance Criteria

- [ ] A `grant → revoke` frame sequence yields a cap span with start = grant `t`,
      end = revoke `t`, `cap.revoked = true`, name = the object name.
- [ ] A granted-but-never-revoked cap yields, at flush, a cap span ending at the
      last-seen timestamp with `cap.revoked = false`.
- [ ] A transitive revoke (parent revoked) closes the parent's span **and** every
      descendant's span, all ending at the revoke `t`.
- [ ] `parent_cap_id` linkage is preserved: a derived cap's span parents onto its
      source cap's span; a root grant (`parent_cap_id == 0`) has no parent.
- [ ] Reply caps (`CapObject::Reply`) produce no cap span.
- [ ] Each cap span carries OTLP `events` for its `granted`/`transferred`/`revoked`
      moments, each timestamped and tagged with the `holder` at that moment.
- [ ] Cap spans carry the `capabilities` trace_id, distinct from the session trace.
- [ ] Existing session-span export is unchanged (all current collector tests green).

## Steps

Every step follows RED-GREEN-MUTATE-KILL MUTANTS-REFACTOR. No production code
without a failing test. Host-tested via `cargo test -p collector` throughout.

### Step 1: Extract a pure `build_proto_span` seam from the OTLP exporter

**Acceptance criteria**: `Exporter::export` builds its `Span` proto via a pure,
host-testable `build_proto_span(&CompletedSpan, trace_id) -> Span`; a test asserts
the proto's `trace_id`, `span_id`, `parent_span_id` (empty when parent == 0), name,
start/end nanos, and attribute set — with **no** HTTP. No behaviour change; the
`#[mutants::skip]` stays on the HTTP-making method only.
**RED**: Test `build_proto_span` maps a `CompletedSpan` onto the expected proto
fields (currently that logic is inline in the HTTP-skipped `export`, untestable).
**GREEN**: Extract the inline builder into `build_proto_span`; `export` calls it.
**MUTATE / KILL MUTANTS**: Cover parent-empty vs parent-set and the nanos clamp.
**REFACTOR**: Only if the extraction exposes duplication.
**Done when**: Seam extracted + tested, existing tests green, human approves.

### Step 2: Carry a trace discriminator, free-form attributes, and span events on `CompletedSpan`

**Acceptance criteria**: `CompletedSpan` gains `trace: TraceKind` (`Session` default
| `Capabilities`), `extra_attributes: Vec<(String, AttrValue)>` (default empty), and
`events: Vec<SpanEvent>` (default empty; each `SpanEvent { name, time_ns,
attributes }`). `Exporter` holds a second `cap_trace_id`; the `Span` proto gains an
`events` field (a `SpanEvent` OTLP message); `build_proto_span` selects the trace_id
by `trace`, appends `extra_attributes`, and maps `events` onto the proto. Session
spans (default `Session`, empty extras/events) produce byte-identical protos to
Step 1.
**RED**: Test that a `CompletedSpan { trace: Capabilities, extra_attributes: [..],
events: [granted@t, revoked@t] }` builds a proto carrying the cap trace_id, the
extra attributes, **and** the two span events; and a default span still builds with
the session trace_id, no extras, no events.
**GREEN**: Add the fields (defaulted), the second trace_id, the `SpanEvent` proto
message, and the selection/append/event-mapping in `build_proto_span`.
**MUTATE / KILL MUTANTS**: Cover the trace selection branch, the attribute append,
and the event mapping.
**REFACTOR**: Assess `span_attributes` vs `extra_attributes` merge point.
**Done when**: Criteria met, session path unchanged, human approves.

### Step 3: Reconstruct cap holdings into cap spans (pure `collector/src/caps.rs`)

**Acceptance criteria**: A `CapTracker` ingests cap-event data
(`kind, cap_id, parent_cap_id, holder, object, rights, badge, t, name`) and:
(a) on `Revoked`, emits closed cap spans (start = grant `t`, end = revoke `t`,
`revoked = true`) for the revoked cap **and its descendants**; (b) `flush(now_t)`
emits still-open holdings (end = `now_t`, `revoked = false`); (c) drops
`CapObject::Reply`; (d) roots (`parent_cap_id == 0`) carry no parent; (e)
accumulates each cap's `granted`/`transferred`/`revoked` moments as `SpanEvent`s
(timestamped, `holder` attribute) and attaches them to that cap's emitted span.
Each emitted item maps to a `CompletedSpan { trace: Capabilities, span_id: cap_id,
parent_span_id: parent_cap_id, name: label, extra_attributes: [cap.*], events:
[..] }`. Pure, fully host-tested; not yet wired into `State`.
**RED**: Tests for each acceptance-criteria clause (grant→revoke duration; open cap
at flush; transitive-revoke subtree closes together; reply-cap dropped; root has no
parent; label = name-or-kind; the span carries granted+revoked events). Mirror
`diagram/caps.rs`'s conventions.
**GREEN**: Implement `CapTracker` (open-holding map keyed by `cap_id`; child index
for transitive close; per-cap event log) + the `CompletedSpan` mapping. Timestamps
stay kernel-`t` here; anchoring to wall-clock happens in Step 4 via the session
anchor (events anchored alongside the span).
**MUTATE / KILL MUTANTS**: Cover the subtree walk, the reply-drop, the
revoked/held flag, the parent-zero branch, the event accumulation.
**REFACTOR**: Factor shared label/attribute building with an eye toward the
`diagram` conventions (do not couple crates; duplicate-with-a-comment is fine).
**Done when**: Criteria met, mutation report clean, human approves.

### Step 4: Wire `CapTracker` into `State` + flush triggers, drain in `main`

**Acceptance criteria**: `Frame::CapEvent` feeds the `CapTracker` (anchoring `t`
→ wall-clock nanos via the existing `SessionAnchor`) instead of only
`advance_anchor`; `State` exposes `drain_cap_spans() -> Vec<CompletedSpan>` (closed
on revoke) and `flush_caps() -> Vec<CompletedSpan>` (open holdings). The `main` loop
exports drained cap spans after each frame and calls `flush_caps` on a new `Hello`
(kernel restart) and at stream EOF / shutdown. `handle`'s existing
`Option<CompletedSpan>` signature is **unchanged** (no Option→Vec churn).
**RED**: A `State`-level test drives `Hello` → grant → revoke frames and asserts
`drain_cap_spans` yields the expected anchored cap span; a second asserts
`flush_caps` closes an open holding at the last-seen anchor.
**GREEN**: Add the tracker to `State`, feed it from `handle`, add the drain/flush
methods; wire `main` to drain + flush.
**MUTATE / KILL MUTANTS**: Cover the anchor conversion and the `Hello`-reset path.
**REFACTOR**: Assess the `handle` cap arm vs the other deferred arms.
**Done when**: Criteria met, session export untouched, human approves.

### Step 5: End-to-end proof via a capturing exporter

**Acceptance criteria**: An integration test drives a realistic grant→transfer→
revoke `CapEvent` stream (mirroring the `grant fs` / `mint SEND` / `revoke` demo)
through `State` + a fake `SpanExporter` that records spans, and asserts the captured
cap spans form the expected named tree in the `capabilities` trace (edges, names,
durations, revoked flags). Optionally assert the tree matches the shape
`diagram::derivation_tree` produces from the same frames.
**RED**: The capturing-exporter integration test above.
**GREEN**: Add the test-only capturing exporter; assert on the tree.
**MUTATE / KILL MUTANTS**: N/A beyond Steps 3–4 (this is an integration guard).
**REFACTOR**: N/A.
**Done when**: Tree asserted end-to-end, human approves.

## Out of scope (follow-ups)

- **The live "watch it happen" channel (metric/log).** OTLP has no open-span update,
  so the trace view is structure-after-the-fact; a *growing bar* in Tempo isn't
  achievable (re-emitting a stable span_id per tick fights Tempo's append/no-dedup
  and flickers). The real-time signal belongs on the metric/log path, as a **separate
  follow-up subsystem**: a Prometheus `caps_held{object,holder}` gauge (grant
  increments, revoke decrements) and/or Loki lines (`"init granted fs → fs-server"`),
  so Grafana shows authority moving *as it moves*, then you drill into the Tempo tree
  for the named structure. `state.rs` already flags CapEvent→Prometheus as a distinct
  follow-on; this names it as the complement to the trace view. **Out of scope here**
  (different subsystem), but the intended pairing: events-live on metrics/logs,
  structure-on-close in traces.
- **`view a-file`** (grant a scoped cap to a spawned viewer) — the post's *further*
  "what's next"; unblocked by this but separate.

## Pre-PR Quality Gate

Before each PR:
1. Mutation testing — run `mutation-testing` skill on the new pure logic
   (`caps.rs`, `build_proto_span`).
2. Refactoring assessment — run `refactoring` skill.
3. `cargo xtask clippy` + `cargo test -p collector` green.
4. Confirm existing session-span behaviour unchanged.

---
*Delete this file when the plan is complete. If `plans/` is empty, delete the directory.*
