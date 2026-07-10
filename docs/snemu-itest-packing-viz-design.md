# snemu-itest packing visualization — design

## Architecture (decided)

**Two layers, cleanly split:**

- **Data layer (Rust, ✅ done)** — the audit writes a single pretty-JSON snapshot
  per run, `.itest-runs/snemu-packing.json`, mirroring the itest history's
  `capture.json` (format-follows-shape: TOML for committed/PR-diffed baselines,
  NDJSON for append streams, **JSON for a gitignored tool-consumed snapshot**).
  Serde structs `PackingReport`/`WorkerStat`/`Segment` in `xtask`; schema below.
- **Render layer (React + TypeScript, to build)** — a Vite app in a new `viz/`
  workspace (isolated from the Rust build), rendering any `*.json` the audit
  drops. **Single-file build output** (`vite-plugin-singlefile` or a `data.js`
  sidecar) so the artifact stays double-clickable / `file://`-openable — React's
  extensibility without a dev-server to view. Extensible into other snemu viz
  (trace timelines, diff viewers) later; the packing timeline is the first
  component.

The split means the renderer is swappable and the JSON is independently useful.

## Purpose

After each `snemu-itest` run, render a **self-contained HTML page** that *animates*
how the N host workers packed the scenarios across wall-time. It makes the two
things numbers hide visible:

- **Imbalance** — which worker is the bottleneck, and how much idle "tail" the
  others waste after they finish (the utilization gap).
- **The packing win** — default (selection-order) vs LPT, side by side, so the
  shorter tail / tighter lanes are obvious at a glance.

It's the visual companion to the `=== worker utilization ===` report.

## Data model (what the audit emits)

The audit already tracks worker id, per-scenario wall time, and per-group timing.
It needs to additionally record **start offsets** (relative to run start) so bars
can be placed on a timeline. Emitted as one JSON blob (embedded in the HTML):

```json
{
  "packing": "LPT",              // or "selection-order"
  "jobs": 10,
  "makespan_s": 57.3,
  "total_instret": 6531000000,
  "boot_instret": 1347000000,
  "workers": [ { "id": 0, "busy_s": 55.1, "util": 96.2 }, ... ],
  "segments": [
    { "kind": "boot", "workload": "frame-oom", "worker": 1,
      "start_s": 0.00, "end_s": 0.85 },
    { "kind": "scenario", "name": "frame-allocator-oom", "workload": "frame-oom",
      "worker": 1, "start_s": 0.85, "end_s": 38.30,
      "instret": 774000000, "pass": true },
    ...
  ]
}
```

A **boot** segment is the once-per-workload snapshot boot (grey); each scenario is
a **fork+run** segment after it. Both live on the same worker lane, back to back.

## Artifact

- **Data**: `.itest-runs/snemu-packing.json` — the latest run's snapshot,
  overwritten each run (a machine artifact, gitignored). Written unconditionally
  (cheap); timestamped history copies are a later add if wanted.
- **Render**: the `viz/` React app builds to a single-file `dist/index.html` that
  loads the JSON (via a `data.js` sidecar the audit refreshes, or `fetch` when
  served). Double-click to view.

## Visual — swimlane Gantt + sweeping playhead

- **Lanes**: Y axis = workers `w0..w{jobs-1}`, one horizontal lane each. X axis =
  wall-time `0 → makespan`.
- **Bars**: each segment is a rectangle in its worker's lane (`x = start`,
  `width = duration`). Colour = workload (hashed to a stable hue) so a workload's
  scenarios share a colour; **boot** segments grey; **failures** a red hatch.
- **Labels**: scenario name inside the bar when wide enough; hover tooltip shows
  name · workload · instret · wall.
- **Playhead**: a vertical line sweeps `0 → makespan` over a configurable playback
  duration (default ~8 s). Segments **reveal** as the playhead reaches their
  start; an active lane glows; a worker that finishes before the makespan shows a
  **dim idle tail** to the end — the wasted-core gap, animated.
- **Markers**: a solid `makespan` line, and a dashed **ideal** line at
  `total_busy / jobs` (the perfect-packing lower bound) — the distance between
  them is the packing headroom.
- **HUD**: live sim-clock, active-worker count, % complete, and a mini per-worker
  utilization bar column that fills as the run replays.
- **Controls**: play / pause / scrub / speed.

## Comparison mode

Two stacked timelines on a shared X-scale — **selection-order** on top, **LPT**
below — driven from two runs' JSON. The eye immediately sees LPT pull the heavy
bars to the left and shrink the ragged tail. Implementation: the page accepts an
array of runs; if given two, it stacks them and syncs one playhead across both.

## ASCII mock

```
 snemu-itest — LPT · 10 workers · makespan 57.3s · util mean 84% / min 61%
         0s      10s     20s     30s     40s     50s   57s
 w0 |████ frame-oom █████████████████████████████░░░░░| 96%
 w1 |███ demo ███████░ coop-baseline ░░░░░░░░░░░░░░░░░| 74%
 w2 |██ spawn-reap ░ tlb-shootdown ░░░░░░░░░░░░░░░░░░░| 68%
 w3 |███ userspace ██████░░░░░░░░░░░░░░░░░░░░░░░░░░░░░| 71%
 ...
 w9 |█ stitch-repl ░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░| 61%
       ╿ playhead →                        ideal┆   makespan│
   █ active (coloured by workload)   ░ idle tail
```

## Tech

React + TypeScript, Vite, single-file build. SVG for the timeline (≈108 bars over
≤10 lanes — comfortable; Canvas only if it grows), `requestAnimationFrame` for the
playhead. Charting: lean **visx** (typed React-native D3 primitives) or raw SVG;
avoid a heavy chart lib. `viz/` is its own `package.json`/`node_modules`, isolated
from `cargo`.

## Status / next steps

1. **Data layer** — ✅ done: `PackingReport` serde structs + `.itest-runs/
   snemu-packing.json` write, unit-tested for schema. (Real output pending the
   `stitch` crate compiling, since any run builds the kernel.)
2. Scaffold `viz/` (Vite + React + TS + single-file plugin), load a sample JSON.
3. Packing-timeline component: swimlanes + playhead + idle tails + ideal/makespan
   markers + HUD.
4. Later: comparison view (stack two runs), then other viz types.

## Resolved decisions

- **Format**: JSON (per above) — not TOML (that's for committed/PR-diffed
  baselines) or NDJSON (append streams).
- **Comparison**: single-run page that *optionally* stacks a second run's JSON.
- **Write policy**: always write the latest snapshot; gate history copies later.
- **Playback**: fixed ~8 s regardless of makespan (short + long both watchable);
  the real makespan shows on the axis.
