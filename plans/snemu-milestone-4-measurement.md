# snemu milestone 4 — the measurement spine

Make snemu observe *itself*, rigorously, before any optimization work.
This is the load-bearing artifact of the whole snemu arc: every JIT tier
after it (M5, M6, …) is an *episode measured against it*. The guiding
principle is the project's own — **measure first, then tune what you
measured** — the same way the kernel tunes its heap watermark against
heap metrics.

Design + full rationale: [docs/snemu-design.md](../docs/snemu-design.md)
(*Exploration notes → Measurement*, *→ QEMU*, *→ Nested overhead*).

## Why this step

M3 leaves snemu running the itest suite end-to-end with no JIT — the
"working end-to-end" line. Before we spend effort making it faster, we
need numbers that *mean* something: a credible "here's how fast, here's
vs QEMU, here's where the time goes," and an overhead profile precise
enough to tell each JIT tier what to attack and to *prove* it did.
Without this spine, every later speedup post is unfalsifiable vibes.

snemu's determinism is what makes the benchmarking honest: same workload
+ seed → identical guest execution, identical instruction count across
every engine variant; only wall-clock varies. True apples-to-apples
deltas — something QEMU (nondeterministic, no fixed instret) can't give.

## Decisions locked in

| decision | choice |
|---|---|
| Two measurement modes | **measurement** (cheap counters: instret, wall-clock, cache stats; low perturbation — source of speedup numbers) vs **observability** (full per-instruction frames / MMIO / page-fault spans; debugging + demos; accepts slowdown). The observer effect is real. |
| Headline metrics | guest MIPS; wall-clock per itest scenario; host-work-per-guest-instruction; hot-block concentration; block-cache hit rate / dispatch counts; startup time; code-cache + guest RAM size |
| Baseline | QEMU (TCG) per-scenario wall-clock, recorded for the same scenarios |
| Workload taxonomy | startup-bound / compute-bound tight-loop / memory-bound / trap-MMIO-heavy (so "various workloads" has texture + an honest diminishing-returns story) |
| Nested overhead | snemu-under-snemu → overhead factor `H/G` from two instret readings; per-class via bracketed microbenchmarks; exact, deterministic, no host perf counters |
| Output | metrics flow out as `Frame`s → Grafana; a repeatable `cargo xtask snemu bench` harness |
| Cheap counters | already present from M1; this milestone *hardens* them into the two-mode split, it doesn't introduce them |

## Progress (2026-07-07)

- **Step 1 — reframed + partly SHIPPED.** snemu has *no per-instruction
  telemetry emission today* (only aggregate counters), so there is nothing to
  gate — the literal "two-mode split" is premature until observability-mode
  per-instruction tracing exists (deferred to whenever that lands; YAGNI now).
  What was load-bearing and real: exposing the deterministic aggregate counter.
  `Machine::instret()` (returns the shared clock `time` = total retired across
  harts) is SHIPPED, host-tested (determinism + cross-hart aggregation).
- **Step 2 — SHIPPED.** `cargo xtask snemu bench [--workload W] [--steps N]
  [--runs K]` runs a workload under snemu K times, timing only the step loop
  (no per-step decode = no observer effect), and reports guest MIPS (best/mean/
  worst) over a deterministic instret. Determinism is *enforced*: the pure
  `snemu::bench::BenchReport::from_samples` errors if instret varies across runs
  (12/12 viable mutants caught). First number: **default `init` boot ≈ 20 MIPS**,
  instret identical to the instruction across runs. Startup-time split is the
  one Step-2 sub-item still open (measure boot-to-first-frame separately).

### Step 1 — the two-mode split (deferred; see Progress)
- Gate per-instruction telemetry behind a mode flag. Measurement mode
  emits nothing per-instruction — only aggregate counters (instret,
  wall-clock, cache stats). Observability mode emits the rich frames.
- Test: a workload run in measurement mode produces *no* per-instruction
  frames and a stable instret; the same workload in observability mode
  produces the frame stream. Instret identical across modes (determinism
  check — the mode must not change *what executes*).

### Step 2 — the benchmark harness (SHIPPED; startup-split open)
- `cargo xtask snemu bench [workload]`: runs a workload deterministically
  (fixed seed), in measurement mode, N times, reports guest MIPS +
  wall-clock + startup time with variance.
- Test: same seed → identical instret every run; wall-clock reported
  with spread.

### Step 3 — the workload taxonomy
- Define the four workload classes as concrete, checked-in benchmarks:
  - **startup-bound** — boot-to-heartbeat.
  - **compute-bound** — a synthetic tight loop (LCG burner) and/or a storm.
  - **memory-bound** — a load/store-heavy loop.
  - **trap/MMIO-heavy** — a syscall-y scenario.
- Each is a guest program (or existing scenario) with a known, fixed
  instruction budget so cross-engine comparison is exact.

### Step 4 — the QEMU baseline
- Record QEMU (TCG) wall-clock for the same taxonomy + the real itest
  scenarios. Capture the startup-vs-execution split where possible.
- Deliverable: a baseline table snemu variants are measured against.
  Note the determinism asymmetry (snemu seeded once vs QEMU `--repeat`).

### Step 5 — hot-block + dispatch profiling
- In measurement mode, maintain a cheap per-PC (or per-block) execution
  counter → hot-block concentration (what fraction of execution lives in
  the top-N blocks). This *predicts* JIT payoff before building it.
- Test: a tight-loop workload shows high concentration; a flat workload
  shows low.

### Step 6 — the Grafana dashboard
- Wire the measurement metrics out as `Frame`s through the existing
  collector path so they land in Grafana (reuse the kernel's telemetry
  plumbing). One dashboard: MIPS, per-scenario wall-clock, hot-block
  profile, startup time, vs-QEMU overlay.

### Step 7 — nested overhead-factor methodology
- **Marker channel:** a recognizable signal (magic MMIO write the outer
  snemu watches for, or a nop pattern) so the inner snemu can *bracket*
  its measured region — `H` (outer instret delta) excludes inner
  startup/IO.
- **Aggregate factor:** run inner snemu (in measurement mode) under outer
  snemu; `overhead = H / G` where `G` = inner's reported guest instret,
  `H` = outer's bracketed instret delta. Exact, deterministic, no host
  perf counters.
- **Per-class profile:** inner runs targeted microbenchmarks (a loop of
  `add`s; a loop of `ld`/`sd`; a trap/MMIO crossing); read the outer
  instret delta per benchmark → host-instructions per {ALU op, memory
  op (the soft-MMU cost), exit}.
- Test: the per-class profile is stable run-to-run (determinism); the
  memory-op class costs more host-instrs than the ALU class (sanity:
  soft-MMU is heavier than register arithmetic).
- **Cost is a non-issue** — measuring counts, not time; small bracketed
  microbenchmarks (a few million guest instructions) run in seconds and
  give exact numbers regardless of the nested slowdown.

## Acceptance criteria

- `cargo xtask snemu bench` reports deterministic instret + wall-clock +
  startup for every taxonomy workload, with a QEMU baseline alongside.
- Measurement mode emits no per-instruction frames; observability mode
  does; instret is identical across modes (determinism preserved).
- The Grafana dashboard shows MIPS, per-scenario wall-clock, hot-block
  concentration, startup, and the QEMU overlay.
- The nested overhead factor `H/G` is reproducible to the instruction,
  and the per-class profile separates ALU / memory / exit costs.

## What this unlocks

- **M5 (decode cache)** and **M6 (block chaining)** each become a clean,
  deterministic before/after: a wall-clock delta, an `H/G` drop, and a
  specific bar of the per-class profile cratering (ALU-op cost for M5,
  dispatch cost for M6).
- The **algorithmic-vs-wall-clock** cross-check: when a tier drops `H/G`
  but not wall-clock proportionally, that's the microarchitecture tax
  (instructions traded for cache misses / mispredicts) — a finding, and
  a post, almost no hobby emulator can produce.
- Each milestone is a devlog post; M4 is "snemu measures snemu using
  nothing but snemu."
