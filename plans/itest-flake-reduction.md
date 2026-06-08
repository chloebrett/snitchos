# Integration-suite flake reduction

Follow-up to `plans/residual-race-investigation.md`. That investigation
closed the hunt for a single coherent cross-hart race ("Bug B") — it is
**gone**, accidentally fixed mid-investigation. This plan addresses the
**residual ~3.5–5.5% per-scenario jitter** that remains, which is *not*
one bug. It is several small, disparate sources, most of them in the
**test harness and the QEMU environment, not the kernel**.

No code in this document. Plan only.

---

## Where we are (don't re-litigate this)

From `residual-race-investigation.md`, Appendix C and post 16:

- Five storm scenarios (`ipi-pong`, `shootdown`, `spawn`, `mutex`,
  `virtio`) each isolated a hypothesised cross-hart surface and ran
  clean at high N (10k–250k trials). Every originally-suspected
  hypothesis class is falsified.
- Fix-on vs fix-off across the four flaky default scenarios is
  statistically indistinguishable: aggregate **3.75% (fix on)** vs
  **3.5% (fix off)**, every per-scenario p > 0.05. The
  `tag("trap return")` MMIO fence was removed; suite rate did not move.
- Most probable actual fix: the per-CPU lift of
  `TICK_PENDING` / `LAST_IRQ_DURATION` from cross-hart globals.

**Conclusion carried into this plan:** there is no longer a single root
cause. H9 ("the residual is several distinct flakes") was right. The
strategy is therefore *partition the jitter and remove each source*, not
*hunt the race*. The failure mode to avoid is re-opening a race hunt the
data says is finished, or re-adding a blanket MMIO fence that obscures
the true rate (post 16's hardest-won lesson).

---

## The causal model — what the residual is made of

Four contributing sources, in rough order of suspected contribution.

### Cause 1 — The test oracle measures the wrong clock (suspected dominant)

`init_timer(timebase_hz)` arms the heartbeat at `timebase_hz` ticks →
**one heartbeat = exactly one guest-second** (QEMU `virt` timebase is
10 MHz). Scenario budgets are **20–45 host-wall seconds** for events
that occur within 1–2 guest-seconds — 15–30× headroom *in guest time*.

So a blown budget almost never means "the kernel was 30 seconds late."
It means one of:

1. a true wedge — guest time stopped, no frames at all; or
2. the host gave that QEMU process so little CPU that 1–2 guest-seconds
   took longer than the host-wall budget.

Case 2 is a **bug in the harness, not the kernel.** `Harness::wait_for`
(`xtask/src/itest/harness.rs:128`) deadlines on `Instant::now()` —
host wall-clock — while the property under test is *guest progress*.
Under host-CPU pressure a perfectly healthy kernel can miss a wall-clock
budget. This is the H9 "budget-exhausted (kernel alive, just slow)"
signature the investigation flagged but never separated out.

### Cause 2 — Host-CPU contention under parallelism

Post 17 set `--jobs 10 --cpu-jobs 3` as default and adopted the current
`.itest-baseline.toml` **at `--jobs 10`**. The jobs=1-vs-10 confound was
explicitly left as the "next session" TODO and never settled.

The mechanism is proven, not speculative: at `--jobs 20` the suite
showed `deflake-ipi-pong` timing out at **30.1s flat** — a
`wait_for(Duration::from_secs(30))` cliff — caused by a guest vCPU
thread being starved by the host scheduler, never receiving its IPI in
wall-clock time. `-smp 2` × `thread=multi` means each scenario spawns 2
host vCPU threads; 10 wfi scenarios + a 3-wide cpu-bound batch can
oversubscribe a laptop during the cpu-bound pass. This feeds directly
into Cause 1: contention → guest starvation → wall-clock budget blown.

### Cause 3 — Cooperative-scheduling throughput variance (v0.5/v0.6)

Some scenarios (notably `workload-cooperative-baseline`) assert that a
quantity of *work* completes within budget, not just that a frame
appears. Under multi-thread TCG the interleaving of cooperative yields
varies boot to boot; an unlucky interleaving delivers less throughput in
the same guest-time window. This is a real property of the cooperative
v0.5 scheduler, distinct from a memory-ordering bug — the kernel is
correct, it's just occasionally slow to make the assertion's threshold.
H9 caught one concrete instance (metric-registration interleaving 70
virtio sends with workload slices; fixed). Others of this shape may
remain.

### Cause 4 — A genuine kernel residual (evidence says near-zero)

After Causes 1–3 are removed, whatever `fast-exit-wedge` rate survives
is the only part that is a real kernel bug. The evidence is that this is
**at or near zero**: every targeted storm was clean at high N. The one
audit never performed is H8 — hart-0 load-side reads of state hart 1
wrote, outside a critical section. We do this audit **only if** signature
classification (Workstream A) shows a nonzero wedge rate. We do **not**
pre-emptively re-add a fence.

### The environmental root underneath Causes 1–3

All three non-kernel causes share one root: **multi-thread TCG is
nondeterministic** in both scheduling and wall-clock timer delivery, and
the harness is coupled to host wall-clock. `thread=multi` was adopted
(see `qemu.rs:22-29`) because `thread=single` starved whichever hart
wasn't executing, skipping timer IRQs. So we traded determinism for fair
timer delivery. Workstream D asks whether `-icount` can buy back the
determinism without losing the fairness.

---

## Workstreams, in priority order

Ordering rationale: A is a measurement prerequisite (everything else
needs per-failure buckets to be verifiable). B and C attack the two
suspected-dominant causes and are cheap. D is the ambitious
environmental fix with a real caveat. E is gated on A showing it's
needed.

### A — Classify failure signatures automatically (do first)

**Why first:** "5%" is a blended number. Until each failure is bucketed
we cannot tell a kernel flake from a busy laptop, cannot verify that B/C
moved the right slice, and cannot know whether E is even needed. This is
the H9 "signature-classify" experiment that was listed and never run.

**What exists to build on:** the harness already preserves
`fail-<scenario>-<iter>.log` (QEMU stderr/UART) per failure, and
`iterations.ndjson` records result + error string + log filename per
iteration. The classifier is a pure function over the captured evidence.

**Key finding that reshaped this workstream (from reading real logs):**
telemetry leaves over **virtio, not the UART log**, and the kernel does
not UART-log per heartbeat, so every `fail-*.log` looks identical
("…entering heartbeat", silence) — *the log file alone cannot classify*.
The load-bearing signal is whether the frame socket **disconnected**
(QEMU died → wedge) or merely **timed out** (QEMU alive → slow), which
`wait_for` knows at failure time and currently discards. So this
workstream now has two halves: (A1) a richer **failure capture** that
persists the frame evidence, and (A2) the pure **classifier** over it.

**Buckets (refined — `host-contention` collapsed, `Stalled` added):**
single-failure evidence cannot separate host-contention from a tight
budget (both are timeouts with frames flowing); that split is a
*cross-run* correlation with `--jobs`/host load, handled in Workstream C,
not the classifier. What *is* separable per-failure is whether the
kernel went quiet (`Stalled`) vs kept emitting (`BudgetExhausted`).

| Bucket            | Discriminator                                                          | Maps to cause |
|-------------------|------------------------------------------------------------------------|---------------|
| `Wedge`           | socket disconnected, or `Kernel panic:` in log — kernel died           | Cause 4       |
| `Stalled`         | timeout, QEMU alive, but quiet ≥ `STALL_QUIET_MS` before deadline       | Cause 4 (deadlock/spin) |
| `BudgetExhausted` | timeout, QEMU alive, frames flowing up to the deadline                  | Cause 1 / 2 / 3 |
| `Harness`         | harness-tagged infra error, or external `terminating on signal`        | none (infra)  |
| `Unknown`         | insufficient evidence (untagged historical capture)                    | —             |

**Error tagging (robustness):** the harness **stamps every error it
produces with an `ErrorOrigin`** (`Harness` vs `Scenario`) rather than
the classifier guessing infra-vs-kernel from error text. The classifier
trusts the tag; a fragile error-string substring heuristic remains only
as a fallback for untagged historical rows. This prevents a scenario
assertion that coincidentally contains an infra word ("connect", etc.)
from being misattributed.

**Configurable capture depth (records all failures by default):** the
summary record is **always** captured on every failure (it is the
classifier's input — an unattributed failure is a measurement hole). The
frame *transcript* depth is configurable:

| Level | On failure persists | In-memory retention |
|-------|---------------------|---------------------|
| `summary` | summary record only | ring (small) |
| `tail` *(default)* | summary + last ~64 frames NDJSON | enlarged ring |
| `full` | summary + entire decoded stream for that iteration | whole iteration buffered, freed on pass |

Plus `--capture-passes` (off by default) to also persist transcripts for
passing iterations — the heaviest investigation mode. **Constraint:** the
level changes how much the reader thread must retain, so it is known at
`Harness::spawn` time, threaded CLI → runner → harness — not just a
serialization choice.

**Build status (TDD, in `itest-harness/src/signature.rs`):**

1. ✅ DONE — pure `classify(&FailureEvidence) -> Signature`. Nine tests:
   the five buckets, the `ErrorOrigin` tag being authoritative over the
   substring fallback, panic-in-log → `Wedge` on untagged data,
   `Stalled` via `last_frame_wall_age_ms ≥ STALL_QUIET_MS`. `ErrorOrigin`
   /`FailureEvidence`/`Signature`/`classify` exported from the crate root.
2. TODO — `FailureCapture` owned/serializable record (summary fields +
   optional transcript), TDD its (de)serialization; `classify` consumes it.
3. TODO — harness wiring: capture outcome / `frames_seen` /
   `last_frame_wall_age_ms` / per-hart last-`t` / frame histogram +
   `ErrorOrigin` stamping into a `LAST_FAILURE_CAPTURE` thread-local
   (same pattern as `LAST_MAX_WAIT`/`LAST_LOG_PATH`); runner persists per
   the capture level; enlarge the `recent` ring; add the `--capture` /
   `--capture-passes` flags.
4. TODO — surface the signature in `iterations.ndjson` (new field), the
   end-of-run summary, and a per-bucket column in the baseline
   aggregation so Grafana breaks the rate down by bucket.

**Done when:** a `--repeat 50` run reports the residual split into the
buckets per scenario, persisted to NDJSON and visible in the existing
itest dashboard.

**Prediction:** `BudgetExhausted` dominates; `Wedge` + `Stalled` are
≤ 1% suite-wide and possibly 0.

### B — Guest-time budgets instead of host-wall budgets

**Why:** the cleanest root-cause fix for Cause 1. Re-anchors the oracle
to the clock the property actually lives on. A healthy-but-starved
kernel passes (correctly); a wedged kernel still fails.

**Design:** frames carry kernel `t` (guest ticks); `Harness` already
tracks `timebase_hz` (`harness.rs:185`). Add a guest-time-relative wait:

- Primary deadline: "N heartbeat-intervals of *guest* time have elapsed
  (measured from frame `t` deltas) without the awaited frame arriving."
  Guest time only advances when the kernel runs, so host starvation
  cannot trip it.
- Backstop: keep a generous **host-wall** hard cap (e.g. existing 45s)
  purely to catch a true wedge where guest time itself has stopped — no
  frames means no `t` advancing means the guest-time deadline never
  fires, so the wall cap is the only thing that can end the wait.

**Build (TDD, in `itest-harness` for the pure deadline logic, then wire
into `Harness::wait_for`):**

1. Extract the deadline decision into a pure, host-testable function:
   `should_give_up(last_frame_t, first_frame_t, timebase_hz,
   guest_budget_ticks, wall_elapsed, wall_cap) -> bool`. TDD it directly
   — this is the seam.
2. Replace the `Instant`-only loop in `wait_for` with one that consults
   both the guest-time function and the wall cap.
3. Convert the existing `SEC * N` budgets to
   `guest_intervals(N)` + a shared wall cap. Keep the cap generous; the
   guest-time term is now the meaningful one.

**Done when:** the four flaky scenarios run clean under deliberately
induced host load (e.g. `--jobs` high enough to oversubscribe) that
previously produced `host-contention` / `budget-exhausted` failures, and
a real wedge (inject one behind a feature flag, e.g. infinite-loop in
heartbeat) still fails via the wall cap.

**Prediction:** the `host-contention` and a large part of the
`budget-exhausted` bucket from Workstream A go to zero. Cause-3
throughput failures (work-not-done-in-guest-window) may remain — those
are real and addressed per-scenario, not by this change.

### C — Settle the parallelism confound

**Why:** Cause 2 is proven to exist (the jobs=20 cliff) but its
contribution at the default jobs=10 is unmeasured, and the baseline was
adopted under it. Cheapest experiment in the plan.

**Experiment (no new code — uses existing stress mode + verdict):**

```
cargo xtask itest <scenario> --repeat 200 --jobs 1   # per flaky scenario
cargo xtask itest <scenario> --repeat 200 --jobs 10  # compare
```

Run for the four flaky scenarios (and `frame-allocator-oom`, the post-17
watch item left explicitly unresolved). Read the verdict block.

**Outcomes:**

- Rate materially lower at jobs=1 → Cause 2 is real and load-bearing.
  Mitigation: pin CPU-bound scenarios to dedicated cores for measurement
  runs, or lower `--cpu-jobs`, or measure flake baselines at jobs=1
  while keeping jobs=10 for speed during development. Document the
  split: *speed runs at 10, baseline/flake-measurement at a width that
  doesn't oversubscribe.*
- Rates consistent → Cause 2 is innocent at jobs=10; parallelism is not
  buying flakes and we stop suspecting it.

**Done when:** every flaky scenario has a jobs=1 vs jobs=10 verdict at
N≥200, and `.itest-baseline.toml` carries a note on which job width its
flake rates were measured at.

**Note on interaction with B:** if B lands first, C's jobs=1-vs-10
difference should *shrink toward zero* — that's a clean confirmation
that B fixed the host-contention oracle bug rather than masking it.

### D — `-icount` determinism spike (timeboxed)

**Why:** the deepest lever — it attacks the environmental root
(nondeterministic TCG + wall-clock-coupled timing) instead of working
around it. `-icount shift=N` makes guest time a deterministic function
of retired instructions, decoupled from host scheduling. That would
neutralise the timer-fairness problem that forced `thread=multi` *and*
the host-starvation problem (Cause 2) at the source.

**The caveat — verify before investing:** historically `-icount`
constrains TCG to a single host thread (deterministic execution can't
coexist with free-running multi-thread vCPUs). We adopted
`thread=multi` specifically because `thread=single` starved harts under
`-smp 2` (`qemu.rs:22-29`). So icount may reintroduce exactly the
starvation we escaped — *unless* icount's deterministic instruction-
budgeted scheduling delivers timer IRQs fairly even while serialized
(it schedules by instruction count, not host time, so the
"hart 1 dominates emulation" failure may not recur). This is the open
question the spike answers.

**Spike (no commitment):**

1. Add `-icount shift=auto` (or a fixed shift) to a *copy* of
   `base_command` behind an env/flag. Do **not** change the default.
2. Run the full suite. Measure: does it boot? does the secondary hart
   get fair timer delivery (i.e. does `thread=single`'s original
   starvation symptom return)? wall-clock cost vs current.
3. If it boots fairly and is fast enough: run `--repeat 200` on the
   flaky scenarios. Determinism should drive the non-kernel flake
   buckets toward zero by construction.

**Done when:** a one-paragraph finding — "icount+SMP is viable here and
deterministic, adopt it / costs too much wall-clock, defer / starves the
secondary, not viable." Either way it's a documented dead-or-alive
answer, not an open question.

**Prediction (from the investigation's tone):** plausibly not viable
with `-smp 2` at acceptable speed, but high enough payoff to be worth
the timeboxed check. If viable it could subsume B and C.

### E — Genuine kernel residual (gated on A)

**Why gated:** only run this if Workstream A shows a nonzero
`fast-exit-wedge` rate after B/C/D have removed the oracle and host
noise. The evidence is that this bucket is empty.

**If needed — H8 hart-0 load-side audit:** walk `kmain` post-secondary-
bringup and the heartbeat path; list every load that depends on
something hart 1 wrote and is read *outside* a critical section (so the
mutex Acquire/Release doesn't cover it). Each is a candidate stale-read
under multi-thread TCG. Output a list with proposed *local, targeted*
fences — never a blanket trap-exit fence.

**Hard constraint (post 16's lesson):** do not re-add a generic MMIO
fence. It makes every trap slower and obscures the real rate. Targeted
experiments at high N beat blanket fences. If a single load is the
culprit, the fix is a fence at that load, justified by a storm scenario
that reproduces and then closes the flake.

### One specific watch-item — `sched-span-survives-yield`

The only scenario recurrently *elevated* above the noise band (10% on
one N=50 fix-off run, 6% on re-run, vs 2.5% baseline; everything else
sits 0–5%). It exercises the most intricate control-flow path in the
suite — per-task `SpanCursor` correctness across a yield
(SpanStart → ContextSwitch(leave) → ContextSwitch(return) → SpanEnd with
matching span id). If any scenario has a scenario-specific bug rather
than shared jitter, it's this one.

**Action:** dedicated `--repeat 200 --jobs 1` plus a Workstream-A
signature read **before** assuming jitter. If its failures are
`fast-exit-wedge`, it's the entry point for Workstream E. If they're
`budget-exhausted`, Workstream B should absorb it.

---

## Sequencing

```
A (classify)  ──┬──>  B (guest-time budgets)  ──┐
                │                                 ├──> re-baseline at high N,
                ├──>  C (jobs confound)  ─────────┘     read per-bucket rates
                │
                └──>  watch-item: sched-span-survives-yield deep-dive

D (icount spike) runs in parallel — independent, timeboxed, may subsume B/C.

E (kernel residual) only if A shows fast-exit-wedge > 0 after B/C/D.
```

A first (everything needs the buckets). B and C are independent and
cheap — do both. D is an independent spike. E is contingent.

## What "done" looks like

One of:

- **Best case:** B + C drive the non-kernel buckets to ~0; the residual
  is dominated by Cause-3 cooperative-throughput variance, addressed
  per-scenario; `fast-exit-wedge` is 0. Suite flake rate falls to "true
  kernel correctness" floor, which the data suggests is near zero.
- **D wins:** icount+SMP proves viable; determinism removes Causes 1–2
  wholesale; we adopt it and retire the wall-clock budget workaround.
- **Documented floor:** some irreducible cooperative-scheduling jitter
  remains, fully attributed by signature, with each bucket either fixed
  or explained. "5% flake" becomes "≤X% Cause-3 throughput variance,
  everything else removed" — a known quantity, not a mystery.

The deliverable is a suite whose flake rate is *attributed*, not just
*measured* — every remaining failure has a bucket and a reason.

## Anti-goals (explicit, from the investigation's scars)

- **No blanket MMIO/SeqCst fence** re-added as a "fix." It masks rather
  than fixes and obscures the rate.
- **No race hunt** for a coherent Bug B — it's gone; the storms proved
  it. New race suspicion requires a `fast-exit-wedge` bucket with
  nonzero rate (Workstream A) *first*.
- **No falsification claims at `--repeat 20`.** CI on 0/20 is
  [0%, 16.8%]; it cannot rule out a 5% rate. Use `--repeat 50` to
  measure, `--repeat 100–200` when a comparison is load-bearing
  (post 16 memory).
