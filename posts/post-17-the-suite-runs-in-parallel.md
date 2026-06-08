# Post 17 — The suite, transformed

> Took the integration suite from ~60s wall-clock to ~15s by running scenarios in parallel — but the parallelism is the most visible piece, not the most interesting one. Along the way: the suite became its own observability project (per-iteration NDJSON history, failure-log preservation, Prometheus textfile + live OTLP exports, a provisioned Grafana dashboard); learned to survive being interrupted (pending sidecars, recover / promote / discard / retroactive adoption); and stopped killing other people's QEMU processes (a file-based mutex replaced the old "murder anything you see" cleanup). Along all of *that*: a 500× mental-model error about what was actually slow, an A/B test that didn't need its own harness, a tokio question that deserved a "no," and a stress-test mode that answered the right question — about a scenario that turned out not to be the right culprit. The empirical sweet spot for `--jobs` landed exactly where the *corrected* mental model predicted.

## what this post was supposed to be

Post 16 ended saying the next post would be step 11 — workload consumer to hart 1, real cross-hart `Mutex<VecDeque>` contention, the long-promised SMP payoff. That arc is still on. But there was a tooling tax I'd been quietly paying for months: `cargo xtask itest` took ~60 seconds for one round of all 24 scenarios. At `--repeat 50` for an A/B comparison that's 50 minutes wall-clock. I'd been treating this as "QEMU is slow, what are you gonna do," and just accepting it.

A weekend later: 24 scenarios in ~15 seconds. But the parallelism is the most visible piece, not the most interesting one. While I was about to dig in on jobs counts and worker pools, I noticed the suite had been generating piles of structured test data per run that I was throwing away on the floor, that an interrupted run lost everything, and that `xtask itest` was happy to send `SIGTERM` to any other `qemu-system-riscv64` it found running — including my `xtask boot` debug session three tabs over. This post is about all of it.

## the suite becomes its own observability project

SnitchOS's whole pitch is "the kernel snitches on itself." The integration suite that exercises the kernel had, until this session, snitched on nothing. Each run printed pass/fail lines to stderr, wrote nothing to disk, exposed nothing to Prometheus. The irony was on me.

The first layer that went in was **per-run history directories**. Every `cargo xtask itest` invocation now creates `.itest-runs/<UTC-timestamp>/`. Inside:

- `metadata.toml` — commit, requested `--repeat`, fail-fast threshold, scenario list, hostname. Written once at run start; never modified.
- `iterations.ndjson` — one append-only JSON row per scenario invocation. Fields: iteration number, scenario name, started-at, duration ms, result, error string, log filename. Append-and-flush per row, so a `kill -9` mid-run loses at most the last in-flight scenario's row.
- `fail-<scenario>-<iter>.log` — for any failed scenario, a copy of the QEMU stderr/UART log captured at the moment of failure. Previously these tail-printed to terminal and were gone the moment the next scenario started.

Two clean wins from this. Failure diagnostics are *persistent*: I can come back to a flake from yesterday, open `.itest-runs/.../fail-heartbeat-cadence-47.log`, and see exactly what the kernel was saying when it died. And the NDJSON is a streamable, schema-stable record of per-iteration timing data — every observation the runner makes, in a format you can `jq` or feed into anything.

Which surfaces the obvious next question: this is observability data, and we have an observability project. Why isn't it in Grafana?

The second layer answered that. The baseline file's per-scenario state (runs, failures, mean / p95 duration, Wilson-score 95% CI bounds, partial-flag, recorded-at timestamp) became nine gauges, exposed via two transports:

- `--export-prom <PATH>` — Prometheus textfile-collector format. Atomic write (tmp + rename) so `node_exporter --collector.textfile.directory=` never reads a half-written file. Works offline, requires no docker stack.
- `--push-otlp [ENDPOINT]` — live OTLP/HTTP push, default endpoint `http://127.0.0.1:9090/api/v1/otlp` (matching the bundled stack's Prometheus container, which now boots with `--web.enable-otlp-receiver` so it ingests OTLP at `/api/v1/otlp/v1/metrics`). Optional explicit endpoint URL overrides.

And then the obvious-in-retrospect step: **auto-push at end of run**. Every `xtask itest` now probes the local OTLP receiver after the suite finishes; if it's reachable, push the canonical baseline; if not, *warn — not silent —* with how to enable it. The cost of being noisy on a missing stack is a one-line stderr; the cost of being silent was that I'd forget to push and the Grafana panels would lie about the freshness.

The third layer is the **provisioned Grafana dashboard**, auto-loaded by the same provisioner that already feeds the other SnitchOS dashboards. Twelve panels:

- A stats row: scenarios tracked, scenarios flaking (>0% rate), pending baselines (>0 = something needs `--promote-pending` attention), hours since the baseline was last updated.
- Three top-10 flake-offender bargauges side-by-side — perceived rate, Wilson lower bound, Wilson upper bound. Reading them together is the diagnostic: rate-high + lower-bound-high = real, confident flake; rate-high + lower-low + upper-high = small sample, statistically inconclusive.
- A top-10 slow-scenarios bargauge (p95 duration).
- A flaking-above-1% table, joining rate / CI bounds / runs / failures across five PromQL queries with a `joinByField` transformation, sorted by Wilson lower bound descending.
- A failure-rate-over-time timeseries showing how each scenario's baseline has moved as it's been updated. Step changes signal regressions; gradual drift signals a flake that hasn't been fixed yet.
- Mean and p95 duration timeseries side-by-side, watching for tail growth even when the mean is steady.

Closing the loop: the test suite that exercises the observability project is now itself observed through the project. The same Tempo + Prometheus + Grafana stack that watches the kernel watches the kernel's CI.

## the suite learns to survive being interrupted

The second tax I'd been paying for months: any `cargo xtask itest --repeat 200 --update-baseline` I started, I had to finish. Ctrl-C at iteration 150 would either lose the data or — worse — partially-write the canonical baseline file. The mitigation I'd been using was "don't Ctrl-C." Not a serious answer.

What landed:

- **`.itest-baseline.toml.pending` sidecar.** First Ctrl-C during `--update-baseline` doesn't kill the run; it sets an atomic flag, the runner finishes the current iteration, then writes the partial result to `.toml.pending` with a `[partial]` marker carrying requested-runs, interrupted-at timestamp, and the run-dir name for traceability. Second Ctrl-C in the handler force-quits without writing anything. The canonical file is never touched mid-run.
- **`--promote-pending` / `--discard-pending`.** Promote moves the pending sidecar into the canonical position, pushing the previous canonical entries into per-scenario history (so you don't lose your old measurement). Discard just deletes the sidecar.
- **`--recover-pending <RUN_DIR>`.** If the in-process pending write got lost — process killed by something other than Ctrl-C, disk full, whatever — rebuild it from the NDJSON. The history layer's tier-2 file *is* a recovery substrate for the tier-1 baseline.
- **`--adopt-run [RUN_DIR]`.** The retroactive one. Ran a `--repeat 200` without `--update-baseline` and decided afterwards you want to keep it? `cargo xtask itest --adopt-run` picks the most recent `.itest-runs/<ts>/`, aggregates its NDJSON, and writes the canonical baseline directly — previous entries pushed to history, no partial marker because adoption is a deliberate promotion. Optionally accepts an explicit path.

The conceptual move underneath all of this: **canonical baseline, pending sidecar, and per-run history are three tiers of the same data, with explicit lifecycle commands moving between them**. You can crash, interrupt, forget to update, or change your mind, and the data flow has a documented path from "what happened" to "what's now the canonical comparison floor."

And a smaller but important fix: **the suite stopped killing other people's QEMU processes**. The old `kill_stale_qemus()` would `SIGTERM` any `qemu-system-riscv64` it found running, on the theory that it was leftover cruft from an earlier crashed run. In practice it would also terminate `xtask boot` debug sessions, manual QEMU invocations, and the GDB-attached session you'd been holding open for two hours. Replaced with two things:

- **`ItestLock`** — a `flock`-based per-checkout mutex at `.itest.lock`. The first `xtask itest` invocation acquires the lock; subsequent ones print the holder's PID and exit. `--force` bypasses if you know the lock is stale. No process-killing involved.
- **`detect_stale_qemus()`** — non-destructive. Detects external `qemu-system-riscv64` processes, warns about them ("probably from `xtask boot`/`xtask debug` or a manual invocation; cross-test interference is possible; kill them manually if needed"), and proceeds without touching them.

The combination: itest-vs-itest collisions are prevented by the lock; itest-vs-anything-else is the user's call to make explicitly.

## the 500× mental model error

The first instinct I took to the design doc was that QEMU emulating riscv64 was so dog-slow on host CPU that there was no point in oversubscribing. I literally wrote, in the first draft of the parallel plan:

> a single QEMU TCG instance pegs one host core at ~100% the entire time it's running — the host is doing dynamic translation as fast as it can, and 10 MHz is what comes out the other end. So if you spawn 2 QEMUs on 1 core, the OS time-slices them at ~50% each, and each guest's effective rate drops to ~5 MHz.

This was wrong in a way I didn't catch until my collaborator pushed back: *"surely qemu can run faster than 10mhz. this is like a 500x slowdown factor. overhead would be more like 50%?"*

The 10 MHz number is the QEMU `virt` machine's `mtime` frequency — what the *guest* reads from its timer, configured in the device tree. It's not the translation rate. Real cross-ISA TCG slowdown vs native is more like 10–50×, not 500×. A Mac core can comfortably translate hundreds of millions of guest instructions per host second.

Which changes the whole picture.

Most of our scenarios are **wfi-bounded**: the kernel boots, sets up a timer interrupt, then `wfi`s between heartbeats. During `wfi` the QEMU vCPU thread parks — host CPU is idle. The wall-clock cost of "wait for 5 heartbeats" is gated by `mtime` advancing, not by translation throughput. Five seconds of wall-clock while consuming almost no host CPU.

A minority are **CPU-bounded**: `heap-oom` scribbles 16 MiB/heartbeat, the sched-yield scenarios spin in context-switch loops, the deflake storms hammer subsystems. These actually consume host CPU at near-100% during their active windows.

So the design implication flipped: *most of our scenarios are nearly free to multiplex*. The right `--jobs` value is bounded by "how many CPU-bound scenarios are running simultaneously," not "how many host cores are there." The earlier-me would have shipped `--jobs num_cpus/2` as conservative. The corrected mental model said the safe default was much higher.

## the worker pool

`std::thread::scope` + `Mutex<VecDeque>` + per-worker `Aggregator` merged at the end. ~80 lines including the cpu-vs-wfi partition. No tokio. No rayon. No crossbeam.

The agent floated tokio at one point — *"this worker queue pattern is starting to look a bit like tokio. thoughts on bringing in a concurrency library so we don't reinvent the wheel?"* — and I pushed back. tokio is the wrong shape for this work: QEMU is blocking I/O, and tokio's async runtime doesn't make blocking work non-blocking, it just funnels it into `spawn_blocking` which is threads with extra steps. We'd get ~100 KLOC of dependency surface and a ~30s cold-build hit for no actual win.

rayon would be a closer fit (fork-join data parallelism), but it prefers chunked work-distribution over a fixed worker pool pulling from a shared queue, which is what we want for fail-fast precision and per-worker accumulators.

What we have is small, ours, and exactly the std API designed for the job. Bringing in a library would have cost more than it saved.

There's a partition step worth naming: scenarios marked `Scenario::cpu_bound` run in a second pass at `--cpu-jobs` width (default `max(1, --jobs/2)`), after the wfi batch finishes. They get their own host core each instead of contending. Initially I had this as a strict serial pass — one CPU scenario at a time. The collaborator's nudge was: *"maybe parallelize half as much as the others?"* The half-as-wide compromise turned out to be the right shape — each Cpu worker still tends to land on its own host core when `jobs ≤ num_cpus`, but the batch doesn't drag the suite down.

## the A/B that already existed

The plan called for a "step 6: A/B harness" — collect rates at `--jobs 1` and at `--jobs N`, run two-prop tests, decide whether the default flip was safe.

I was about to start building it. The collaborator: *"we kind of already have an A/B harness though, right? we just need to run and compare to baseline."*

They were right. The `verdict` module already does pooled two-proportion z-tests against `.itest-baseline.toml` and prints per-scenario `Consistent` / `Worse` / `Better` verdicts at the end of every run. The "A/B harness" was three flags away:

```
cargo xtask itest --update-baseline --repeat 200 --jobs 1   # baseline
cargo xtask itest --repeat 200 --jobs N                     # B-run
```

Read off the verdict block. Any `WORSE` at p<0.05 means parallelism shifted that scenario's rate. The whole thing is a procedure, not a piece of code.

This kept coming up as a pattern over the session: *what existed was enough, I just hadn't seen the shape*. The same thing happened with `--adopt-run` (retroactively promote a completed run as the new baseline) — I had `BaselineFile::from_recovered` already, used by `--recover-pending`. The "adopt" command was the same machinery with the partial-marker stripped and the canonical path as the target.

## the numbers

Real measurements from the dev box. Pre-default:

```
--jobs 1                          ~60s wall, ~60s CPU       (1.0× parallelism)
--jobs 4                          ~20s wall, ~50s CPU       (2.5× parallelism)
--jobs 8  --cpu-jobs 3            ~15s wall, ~51s CPU       (3.4× parallelism)
--jobs 20                         ~37s wall, ~192s CPU      (5.2× parallelism, 5 failures)
```

The cliff at `--jobs 20` is exactly the mental-model prediction: wfi scenarios scale almost free, but the cpu-bound batch oversubscribes and starts contending. Five scenarios timed out — including `deflake-ipi-pong` at 30.1s flat, which is a number that smells exactly like a `Harness::wait_for(Duration::from_secs(30))` cliff. Under contention, ipi-pong (which exercises inter-hart IPI delivery) probably had one of its guest vCPU threads starved by host scheduling and waited forever for an IPI that wasn't getting delivered. Classic guest-deadlock-from-host-starvation. The harness sees a wall-clock timeout.

The empirical answer: jobs=8–10 is the sweet spot on this hardware. Pushed `--jobs 10 --cpu-jobs 3` against a freshly-adopted 40-iteration sequential baseline. 50 iterations Wfi-only. All 16 scenarios came back `consistent` at p > 0.12. Set the default to 10.

Three months of `--repeat 200` will tell me whether the default is right. For now, the suite's a usable yardstick at 4× the throughput.

## stress mode

The original plan deferred "single scenario × N iterations in parallel" — the riskier flavour of fan-out. After the per-iteration parallelism was working, the collaborator asked for it explicitly: *"is it possible yet to run a single case in parallel with itself? would be good to stress test frame-allocator-oom."*

It wasn't. Two blockers: `socket_path_for` / `log_path_for` were `(scenario, pid)`-keyed, so two iterations of the same scenario in the same process stomped each other; and the parallelism barrier was per-iteration, so single-scenario × N at `--jobs 10` was effectively serial.

~50 lines of fixing. A process-wide atomic counter for path discriminators (orthogonal to stress mode, but the fix unblocked it). A "stress-mode" branch in the runner that activates when `scenarios.len() == 1 && repeat > 1 && jobs > 1`: seed the work queue with `(scenario, iter_idx)` for all iterations, fan out at `--jobs` width, merge the per-worker aggregators at the end.

Fail-fast is preserved via an `AtomicBool` stop flag set when cumulative failures cross threshold. Interrupt works the same way. The one invariant break: `run_totals` ordering reflects worker-merge order instead of iteration order — but the per-scenario stats and total failure count are correct, only the per-iteration table is permuted.

## what stress mode told me about frame-allocator-oom

I'd flagged `frame-allocator-oom` after the first A/B as the watch item — 3/50 fails at `--jobs 10` (6%) vs 0/40 baseline at `--jobs 1`. The CIs overlap; p=0.12 didn't cross the threshold; but it was the highest-rate new appearance.

Ran the stress mode against it: `cargo xtask itest frame-allocator-oom --repeat 200 --jobs 10`.

```
=== baseline comparison ===
frame-allocator-oom:
  current  9/200  (4.5%, 95% CI [2.4%, 8.3%])
  baseline 3/50   (6.0%, 95% CI [2.1%, 16.2%]) at ee4ce32
  timing   6.2s mean (p95 6.6s) vs baseline 6.3s mean (p95 6.6s)
  verdict  consistent (p=0.66)
```

Three readings:

1. The stress mode worked. 200 iterations of one scenario at `--jobs 10` ran cleanly, produced a tight CI that excludes zero. The scenario *does* flake, at something like 2.4–8.3% — that's now a measurement, not a guess.
2. Whatever it is, more samples haven't made it worse. The rate at n=200 is *lower* than the n=50 point estimate. It's not "creeping up under load."
3. **You can't actually prove parallelism causes it from this data.** The baseline I adopted was already at `--jobs 10`. To distinguish "always 4.5% flaky" from "0% sequential, 4.5% parallel" would require `--repeat 200 --jobs 1` on this scenario — ~7 minutes wall-clock sequential. I haven't done that yet. My guess is the scenario is just genuinely 4-5% flaky and parallelism is innocent. The cost of being wrong is small; the cost of being right and not knowing is a real ~5% flake hiding in the suite.

This is on the list for next session. Either way: the stress mode answered the question it was built to answer.

## what I caught (the discipline-layer beats again)

Same shape as post 16: agent driving mechanics, me driving methodology. The interventions worth flagging this time:

### "surely qemu can run faster than 10mhz"

The biggest one. I'd put `~500× slowdown factor` into the plan as if it were a known fact. The collaborator's pushback was conversational and uncertain — *"this is like a 500x slowdown factor. overhead would be more like 50%?"* — but right. I had to actually check the numbers and rewrite the mental-model section of the plan. The corrected model changed every recommendation downstream: from `--jobs num_cpus/2` to `--jobs 2 × num_cpus`, from "always serial-pass CPU scenarios" to "parallel-half is fine," from "no point oversubscribing" to "wfi scenarios are nearly free."

The lesson: when the agent confidently writes a quantitative claim into a design doc, *that's the time to push back hardest*. Confident + quantitative + nobody asked is the failure mode I needed to internalise.

### "we kind of already have an A/B harness though, right?"

The A/B-already-exists moment. The cost of *not* catching this was building a separate harness — probably ~200 lines of code duplicating what `verdict` already did, with its own subtly-different statistical logic that would have needed independent maintenance. Three sentences of pushback prevented a whole category of waste.

This generalises. Before designing a new piece of infrastructure, the question is always: *what exists that's 80% of this already?* I'd been narrating the plan steps as if they were independent, when half of them were "use the thing we already have."

### "ok to disagree if you don't think this is a good idea"

The tokio question. What I want to flag is the *form* of the question — explicit permission to disagree. The agent's default failure mode is sycophancy: "great idea, let me wire it up." When the collaborator gates the question with "ok to disagree," it shifts the agent into actually evaluating the proposal on its merits. I gave a real "no, here's why" answer. It got accepted.

This is collaboration shape worth replicating. Asking "should we?" gets "yes." Asking "ok to disagree?" gets reasoning.

## what shipped

The pre-existing setup was a single `xtask itest` file that printed pass/fail to stderr. This session built out the layers around it.

**Architecture.**

- **`itest-harness` workspace crate** — host-buildable, no riscv64 anywhere. Owns the platform-pure runner mechanics, aggregator, baseline I/O, statistical primitives, history layer, exports. The pattern mirrors `kernel` / `kernel-core`: `xtask` keeps the QEMU-spawning glue; `itest-harness` keeps the unit-testable logic. 120 host unit tests run in well under a second.
- **Workflow READMEs split.** `xtask/README.md` is now the user-facing workflow guide (every flag, every metric name, the auto-push behavior, clippy caveats). `itest-harness/README.md` is library docs — module map + design rationale, no command-line surface.

**Statistical foundation.**

- **Wilson-score 95% CI** for per-scenario flake rates (lower/upper bounds, closed-form, no external stats deps).
- **Two-proportion pooled z-test** for current-vs-baseline regression detection, with an Abramowitz-Stegun normal-CDF approximation.
- **`verdict` module** that prints per-scenario `Consistent` / `Worse(p)` / `Better(p)` / `NoBaseline` against `.itest-baseline.toml` at the end of every run. Pre-existing in skeleton; carried the A/B without a separate harness this session.

**History layer (tiers 2-3).**

- **`.itest-runs/<UTC-timestamp>/`** per invocation, gitignored. Holds `metadata.toml`, `iterations.ndjson` (append-only, flushed per-row), `fail-<scenario>-<iter>.log` copies on failure.
- **`--prune-runs --keep-last N`** for disk hygiene. Only ISO-8601-shaped directory names match the pattern, so manual files in `.itest-runs/` are safe.
- **`--recover-pending <RUN_DIR>`** — rebuild the pending sidecar from NDJSON if the in-process write was lost.
- **`--adopt-run [RUN_DIR]`** — retroactively promote a completed run as the canonical baseline. Default = most recent.

**Pending baseline workflow (tier 1.5).**

- **`.itest-baseline.toml.pending`** sidecar — first Ctrl-C during `--update-baseline` writes here, never the canonical file. Carries a `[partial]` marker.
- **`--promote-pending`** — accept the partial as canonical, push previous current per-scenario into history.
- **`--discard-pending`** — delete the sidecar.
- **`--baseline-show [--pending] [--include-history] [--flakes-only]`** — render the baseline file with a banner when a pending sidecar exists; the partial-aware view of partial entries shows `PARTIAL: N/M requested, run-dir <name>`.

**Exports to Grafana.**

- **`--export-prom <PATH>`** — Prometheus textfile-collector format, atomic write. Nine gauges per scenario.
- **`--push-otlp [ENDPOINT]`** — live OTLP/HTTP push, hand-rolled protobuf subset (no full `opentelemetry-proto` dep). Default endpoint targets `stack/docker-compose.yml`'s Prometheus, which now boots with `--web.enable-otlp-receiver`.
- **Auto-push at end of every `xtask itest` run** — probes the local OTLP receiver, pushes if reachable, warns (not silent) if not. `--no-auto-push` to silence in CI.
- **`stack/grafana/provisioning/dashboards/snitchos-itest-baselines.json`** — twelve panels auto-loaded by the existing provisioner. Stats row, three top-10 flake-offender bargauges (perceived rate / Wilson lower / Wilson upper), top-10 slow scenarios, flaking-above-1% table joining five PromQL queries, failure-rate and mean+p95 duration timeseries.

**Concurrent-run safety.**

- **`ItestLock`** — `flock`-based per-checkout mutex at `.itest.lock`. PID-stamped for diagnostics; `--force` to override stale locks. Replaced the destructive `kill_stale_qemus()` that used to murder any `qemu-system-riscv64` it could find.
- **`detect_stale_qemus()`** — non-destructive warning when external QEMUs are running, so `xtask boot` debug sessions don't get killed by `xtask itest`.

**Parallelism.**

- **`--jobs N` defaulting to 10** — empirically validated against a sequential baseline. The harness partitions scenarios into Wfi (parallel at `--jobs` width) and Cpu (separate batch at `--cpu-jobs` width). `std::thread::scope` + `Mutex<VecDeque>`, no async runtime.
- **`Scenario::cpu_bound` annotation** — `CpuProfile { Wfi, Cpu }` field on each scenario. 8 of 24 scenarios marked Cpu: the deflake storms, `heap-oom`, `workload-cooperative-baseline`, `sched-yield-round-trips`. Conservative initial classification; refine empirically.
- **`--cpu-jobs N` defaulting to `max(1, --jobs / 2)`** — CPU-bound batch worker count. Override on small CI runners.
- **`--profile {wfi,cpu}`** — filter scenarios by classification, for isolating which batch drives wall-clock or flake behaviour.
- **Stress mode** — single scenario × `--repeat N` fans out across workers when `scenarios.len() == 1 && repeat > 1 && jobs > 1`. Per-iteration path discriminator via an atomic counter prevents socket-path collisions.
- **`--fail-fast K`** in both per-iteration and stress-mode paths. Via `AtomicBool` stop flag, distinct from the SIGINT flag so fail-fast and Ctrl-C produce different exit codes.

**Output quality.**

- **Per-iteration summary line** now reads `24 passed, 0 failed in 15.2 seconds wall time, 51.4 seconds CPU time` — the CPU/wall ratio is the effective parallelism factor, eyeball-able directly from the run output.
- **`[scenario-name]` line prefixes** in the parallel path, `make -j`-style. Sequential mode keeps the old `test X ... ok` inline format.
- **Tail-of-log printed inline on failure** *and* the same log preserved into the run directory as `fail-<scenario>-<iter>.log`. You get the interactive diagnosis and the durable artifact.

**Documentation.**

- **`plans/itest-harness-extraction.md`** — the migration plan for the workspace split.
- **`plans/itest-history-and-pending.md`** — the tier 1/1.5/2/2.5/3 storage model, lifecycle states, Grafana ingestion path (step H).
- **`plans/itest-parallel-scenarios.md`** — the parallelism design, with the corrected mental model after the 500× error was caught. Step-by-step build order through step 7 (default flip), now annotated with the actual empirical numbers.

## what's actually next

Same arc as post 16's "actually next":

- step 11 — workload consumer to hart 1 under genuine cross-hart `Mutex<VecDeque>` contention. The suite is fast enough now that `--repeat 200` for the comparison is ~10 minutes wall-clock, not ~50.
- step 12 — swap `Mutex<VecDeque>` for `heapless::spsc::Queue`. The lock-wait fall-off-a-cliff moment.
- One subtask before either of those: `--repeat 200 --jobs 1 frame-allocator-oom`. Settle the parallelism-vs-scenario-flakiness question definitively before adopting any more baselines at `--jobs 10`.

The footnote for anyone who designed an integration suite, accepted "QEMU is slow" as a fact, and never asked which part of QEMU was slow: the assumption is *expensive*. Forty seconds saved per run becomes a minute saved per CI cycle becomes an hour saved across a feature. The corrected mental model was three minutes of investigation. The thing I'd been paying for, for months, was not asking.
