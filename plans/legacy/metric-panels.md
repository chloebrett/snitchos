# Plan: metric panels (milestone 4)

**Branch**: main (this project works directly on main; the human commits)
**Status**: Active

Follows [telemetry-panels.md](telemetry-panels.md), which gave the page
the guest's *structure*. This gives it the guest's *numbers*.

## Goal

Time-series panels in the page — the Grafana shape, customised further with things
Grafana structurally cannot do.

## The scope correction this starts from

The previous milestone scoped itself to "structure, not series", inheriting
`docs/uart-telemetry-design.md` Decision 4: custom React wins only where Grafana is
*bad*, and loses at line charts. **That was too narrow** (decided 2026-08-26): charts
belong here too, customised beyond what Grafana allows.

**Prometheus remains the store** for the host and board cases — that decision stands
and this plan does not touch it. But the in-tab case has *no backend at all*, by
design, so for the wasm source the page necessarily retains its own series. That is a
consequence of the existing architecture, not a new position on storage.

## What the wire already gives us, and Grafana does not have

Every `Metric` frame carries `value`, a **guest timestamp**, and `hart_id`;
`MetricRegister` declares `Counter | Gauge | Histogram`. From that:

- **A guest-time x-axis.** snemu's clock is its instruction counter and it is
  deterministic, so plotting against guest time makes two runs comparable
  point-for-point. Grafana assumes wall clock and cannot represent this at all.
  *This is the differentiator; it is not a styling exercise.*
- **Every sample, no scrape interval.** What the guest emitted, not a sampling of it.
- **Kind-aware defaults** from the wire — counters as rates, gauges as values —
  without per-panel configuration.
- **Correlation with structure.** The switch graph knows tasks; a task's CPU-time
  series belongs beside it. Grafana has never heard of a task.

## Acceptance criteria

- [x] A panel plots selected metrics over guest time, live, with counters and gauges
      rendered according to their declared kind.
- [x] Series retention is independent of the frame window — a burst of context
      switches cannot evict metric history.
- [x] The chart is readable in dark, with a palette that **passes
      `validate_palette.js`** rather than one chosen by eye.
- [x] Hovering reads out values at a point.
- [x] Switching workload clears the series, as it clears everything else.
- [x] The panels stay within the responsiveness bar the suite already enforces.

## Steps

RED-GREEN-MUTATE-KILL MUTANTS-REFACTOR throughout. Rust unit tests for retention and
projection, Vitest for scales and components, Playwright only for what needs a
browser.

### Step 1: A series store, independent of the frame window

**Acceptance criteria**: samples are retained **per metric name**, each bounded, so
high-volume traffic in one metric (or in frames generally) cannot evict another's
history. Kind comes from `MetricRegister` and is carried with the series. Reported as
`(name, kind, points[])`.
**RED**: tests that samples accumulate per name; that one metric's overflow leaves
another's history intact; that a series outlives a frame-window overflow; that a
metric with no `MetricRegister` still records (kind unknown, not dropped).
**GREEN**: `snemu-wasm/src/series.rs`.
**MUTATE**: `cargo mutants -p snemu-wasm`.
**Done when**: criteria met, report reviewed, human approves commit.

### Step 2: Scales and shape, in plain TypeScript

**Acceptance criteria**: pure functions turning `(points, size)` into path geometry —
domain from data, a nice-ish tick step, clamping, and the degenerate cases that make
charts crash: one point, all-equal values (a flat line, not a divide-by-zero), an
empty series.
**RED**: those cases, each named for the crash it prevents.
**GREEN**: `web/src/scale.ts`.
**Done when**: criteria met, human approves commit.

### Step 3: The chart

**Acceptance criteria**: an SVG line chart per the `dataviz` procedure — form first,
color by job, **palette validated by script**, thin marks, recessive axes, a legend
for ≥2 series, a crosshair readout. Hand-rolled rather than a charting library: the
forms here are line and stat tile, the customisation is the point, and a library
would have to be fought for the guest-time axis.
**RED**: Vitest over the rendered geometry and the readout; the palette validator run
and recorded.
**GREEN**: `web/src/Chart.tsx`.
**Done when**: criteria met, human approves commit.

### Step 4: The metrics panel

**Acceptance criteria**: a fifth tab. Which metrics to show is the real design
question — this guest emits ~60 per heartbeat and sixty charts is not a dashboard.
Curated groups (heap, scheduler, frames, IPC) as the default, plus **click a metric in
the frame tail to pin it as a chart** — which the tail already lists by name, and
which is the customisation Grafana cannot offer because it does not have the tail.
**RED**: Vitest over grouping and pin/unpin.
**GREEN**: `web/src/MetricsPanel.tsx`.
**Done when**: criteria met, human approves commit.

### Step 5: Live, and proven live

**Acceptance criteria**: a Playwright spec boots a guest, opens the metrics tab, and
sees a heap or scheduler series with more than one point; a second asserts the series
clear on workload switch; `yarn measure` stays within the bar.
**Done when**: all acceptance criteria at the top are met; human approves commit.

## Outcome (2026-08-26)

Shipped: `snemu-wasm/src/series.rs`, `web/src/{scale,metrics}.ts`,
`web/src/{Chart,MetricsPanel}.tsx`, and a fifth tab. **149 unit tests, 18 browser
tests, 70 mutants / 0 survivors**, CPU unchanged at 32.5% (charts render only for the
selected group, so the cost does not scale with ~60 metrics).

**The two honesty questions, resolved.** Counters render as rates labelled
`per second (derived)` — the guest never emitted those numbers, and a chart implying
otherwise misreports its own provenance. Two cases that would draw fiction if
unhandled: a repeated guest timestamp (divide by zero) is skipped, and a counter reset
is reported as zero rather than an enormous downward spike that reads as a real event.
Histograms are excluded, and a group holding only histograms does not appear at all —
an empty group button would promise a view that cannot exist.

**Small multiples, not one chart per group.** A group mixes units — bytes beside block
counts in `heap` — and a shared axis is the dual-axis mistake wearing a different hat.
A grid compares *shapes* without claiming the scales are comparable.

**Groups are derived from names, not curated.** `snitchos.heap.bytes_used` → `heap`,
so a metric added to the guest appears without anyone editing a list here — the same
reasoning as the workload picker validating against the kernel's registry rather than
duplicating it.

**The palette was computed.** `validate_palette.js` against this page's *actual*
surface `#0d0f12` (not the reference `#1a1a19`): all five checks pass — lightness
band, chroma floor, CVD separation (worst adjacent ΔE 8.4 protan), normal-vision floor
19.3, contrast ≥3:1.

**A test caught a design fault, not a code fault.** The metric name rendered twice —
figure caption and chart legend. `dataviz`'s rule is that a single series needs no
legend box because the title names it; the chart now omits it, with a test each way.

**And mutation testing found the same shape for the third time on `Decoder`:** a
delegating accessor (`series`, after `durable_len` and `Status::instret`) that no test
exercised *through the decoder*. A pass-through returning nothing leaves every chart
empty while the store beneath it is perfectly correct. Worth stating plainly: **a
delegating method is not covered by its delegate's tests.**

*Light mode is untested* — the page is dark-only today, so the criterion was met for
the surface that exists. A light palette needs its own validator run against its own
surface, not an inversion of this one.

## Open questions

- ~~**Counter rendering.**~~ Settled: rate, labelled derived. See Outcome.
- ~~**Histograms.**~~ Settled: left out, and said so. Rendering them properly —
  buckets as a heatmap or a quantile band — remains open work, not a gap in this.
- **How far back?** 600 points per series was chosen from the observed heartbeat rate
  (~90 seconds each) but **not measured under load**. It is a comfort knob rather than
  a correctness one, and the assumption behind it is the same kind that was already
  wrong once here about the guest's idle task.
- **Pinning from the frame tail** — click a metric name in the tail to pin its chart.
  Scoped into this plan's step 4 and not built: group selection covers the need, and
  pinning is better designed once there is a reason to want two groups at once.

## Pre-PR quality gate

1. Mutation testing — `mutation-testing` skill on `snemu-wasm`.
2. Refactoring assessment — `refactoring` skill.
3. `cargo xtask clippy`, `cargo xtask test`, `cargo xtask links`.
4. `yarn check`, `yarn test`, `yarn e2e`, `yarn measure`.
5. `validate_palette.js` passes for both light and dark surfaces.

---
*On completion, `git mv` this file to `plans/legacy/` and follow the archiving
checklist in [README.md](../README.md) — including the `.rs` doc-path citations
`cargo xtask links` cannot see.*
