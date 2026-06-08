# itest parallel scenarios

Run the integration suite's scenarios concurrently inside one
`cargo xtask itest` invocation. Goal: cut the dev-loop wall-clock by
running ~N scenarios in parallel on N cores. Non-goal (for now):
parallelising iterations *of the same scenario* inside `--repeat N`
— that's a separate, riskier change covered in the "deferred" section.

## What's safe today, what isn't

Probed `xtask/src/itest/harness.rs` while sketching this:

| Resource | Today | Parallel-safe? |
|---|---|---|
| `.itest.lock` | File lock at repo root, blocks two `xtask itest` *processes* | Doesn't fight intra-process threading. No change. |
| Socket / log paths | `/tmp/snitch-itest-<label>-<pid>.{sock,log}` | Safe across different scenarios in one process. Same scenario in parallel would collide — only matters for deferred case B. |
| `Aggregator` | Single-writer, owned by `runner::run` | Becomes multi-writer. Two options: lock, or per-worker local + merge. Merge is cleaner. |
| `HistoryWriter` (NDJSON) | Single-writer, append-only | Lock around `append` — cheap, fires once per scenario completion. |
| Run-directory naming | UTC second precision, one per invocation | Untouched. |
| Kernel ELF build | Once at startup before scenarios fan out | Untouched. |
| Baseline file writes | Once at end of run | Untouched. |
| `INTERRUPT: AtomicBool` | Set by Ctrl-C handler, checked at iteration boundary | Workers each check at scenario boundary. Untouched. |

## Surface

Add a `--jobs N` flag on `cargo xtask itest`. Initial default: `1`
(preserve current behaviour while we shake the harness out). After a
few weeks of confident green CI: change the default. The right
default depends on understanding our scenarios' CPU profile (see
"Mental model" below) but as a first cut: `min(2 × num_cpus,
scenario_count)`. Most of our scenarios are wfi-bounded, so the
OS scheduler can comfortably interleave more workers than there are
cores. The CPU-bound minority gets covered by either weighting in
the scheduler (deferred) or running them in a serial pass before/after
the parallel batch.

`--jobs 0` is invalid (clap rejects). `--jobs 1` is the
"sequential mode" escape hatch — useful when bisecting a flake to
rule out parallelism-induced timing.

## Mental model: what limits scenario wall-clock

This matters because the *bottleneck* sets the right `--jobs` cap,
and the obvious mental model is wrong.

QEMU TCG slowdown vs native riscv64 is roughly 10-50×, not 500×.
The "10 MHz" figure that floats around isn't translation throughput —
it's the QEMU `virt` machine's `mtime` frequency, configured in the
device tree. The kernel reads `mtime` and sees 10 million ticks per
wall-clock second. Translation throughput is something like
hundreds of millions of guest instructions per host second; a Mac
core runs the guest fast enough that the guest is usually waiting on
its own timer, not waiting for translation.

So the host-CPU profile of a scenario is set by what the kernel does
between observable events:

- **Wfi-bounded** (most scenarios). Kernel boots, sets up timer
  interrupts, then `wfi`s between heartbeats. During `wfi` the
  QEMU vCPU thread parks; the host CPU is idle. Wall-clock cost is
  "wait N seconds for N heartbeats", and during those seconds the
  host CPU is mostly free. These multiplex almost trivially — the
  OS scheduler interleaves N of them at near-zero wall-clock cost.
- **CPU-bound** (the minority). Kernel runs real work between
  observable events: `heap-oom` scribbles 16 MiB/tick, the
  sched-yield scenarios spin in context-switch loops. These actually
  consume host CPU. Their wall-clock under contention grows
  proportionally with the number of competing CPU-bound workers.

Two consequences:

1. **The host CPU is rarely the bottleneck for the suite as a
   whole.** Most scenarios sit in `wfi` waiting for `mtime`. The
   real cap on `--jobs` is "how many CPU-bound scenarios are running
   simultaneously", not "how many cores are there".
2. **Earlier observations need re-reading.** "Some QEMUs eat two
   cores" wasn't really "guest needs 2-core parallelism" — it was
   one host core for translation of a CPU-bound guest, plus
   incidental contention from a sibling worker.

So the model the parallel scheduler should encode is "cheap vs
expensive scenarios" — track which ones consume host CPU and budget
those, leave the wfi-bounded ones to multiplex freely.

## Concurrency model

`std::thread::scope` + a bounded `Mutex<VecDeque<WorkItem>>` queue.
No tokio, no rayon — QEMU is synchronous blocking I/O on a Unix
socket, which is exactly what OS threads are for, and pulling either
runtime in costs more in dependency surface than it gives us.

```text
runner::run
├── seed queue with (scenario × repeat) work items
├── thread::scope:
│   spawn `jobs` workers, each:
│   ├── loop:
│   │     pop work item under Mutex; if None or interrupt set: exit
│   │     spawn QEMU, run scenario, capture outcome + duration
│   │     append to local Aggregator
│   │     {NDJSON, log copy}: under shared Mutex
│   │     print [scenario] outcome line (no buffering)
├── merge per-worker Aggregators into one
└── write baseline / pending as today
```

Per-worker local `Aggregator` then merge avoids contention on the
single hot path (every scenario completion). The NDJSON writer is
behind a `Mutex<HistoryWriter>` — locking cost is irrelevant
compared to QEMU boot time (~1 second).

### Output

`make -j` style: prefix every line with the scenario name in square
brackets. Interleaving is fine; readers learn to grep by prefix. No
buffer-and-flush-on-completion — that loses live progress, which is
the whole point of running tests verbose.

```
[heartbeat-cadence] ok (1247ms)
[boot-reaches-heartbeat] FAILED (892ms)
[boot-reaches-heartbeat]   expected SpanStart "kernel.boot", got Hello
[sched-yield-round-trips] ok (1881ms)
```

### Fail-fast under parallelism

Today's semantics: stop scheduling new iterations once N failures
accumulate. In-flight iterations finish.

Parallel: same semantics. When the failure count crosses the
threshold, set an `AtomicBool` that workers check before popping
their next item. In-flight QEMU runs complete and report normally.
Don't try to kill in-flight QEMU on the threshold cross — they're
already in the failure path or about to finish.

### Interrupt under parallelism

Same model: `INTERRUPT.swap(true)` from the Ctrl-C handler. Workers
see it before popping their next item, exit cleanly. Wait for
in-flight QEMUs to finish (each has its own ~2s timeout), then merge
aggregators and write the pending baseline as today.

A second Ctrl-C still force-quits — `process::exit(130)` doesn't
care about live threads.

## Statistics: do parallel runs measure the same thing?

This is the real risk. Two scenarios on different QEMUs but
sharing CPU cache + scheduler quanta could nudge timing-sensitive
assertions enough to change their pass/fail behaviour. The whole
point of the itest harness is measuring flake rates — if parallelism
shifts what we measure, we shipped a regression.

Mitigation: gate the default flip behind an explicit A/B.

**A/B protocol:**

1. Pick a high-`--repeat` baseline target (the existing 200/400/1000
   nightly runs work).
2. Run `--jobs 1 --repeat 200` against a clean checkout, record
   per-scenario rates.
3. Run `--jobs 4 --repeat 200` against the same checkout, record
   per-scenario rates.
4. For each scenario, run `two_proportion_p_value(k1, n1, k2, n2)`.
   If any p-value drops below 0.05 with a worse-direction shift,
   parallelism changed our measurement. Keep `--jobs` opt-in.
5. If all scenarios are statistically consistent: change the
   default.

The `verdict` module already does this for current-vs-baseline. We
can re-use `two_proportion_p_value` directly; no new statistical
machinery needed.

## Things that might bite us

- **Hidden shared state in the harness.** Anything using a static
  buffer (e.g. a `println!` host-side macro that's actually backed
  by a shared resource) could surface bugs that serial execution
  hid. Most likely: the `take_last_*` thread-local slots in
  `harness.rs`. Worth grep-auditing `static` and `lazy_static` in
  `xtask/src/itest/` before flipping the default.
- **QEMU spawn bursts.** Forking N processes in quick succession
  hits the host scheduler. Probably fine on dev machines; CI runners
  with low ulimits might thrash. Worth a smoke test once `--jobs`
  reaches its default value, but not a fundamental concern at our
  scale.
- **CPU-bound scenario collisions.** `heap-oom` and the sched-yield
  scenarios run real guest work and want a host core. Running two
  of them on the same core slows both proportionally, including the
  guest's perception of `mtime` advancing (which is wall-clock,
  not CPU-time, so the guest *waits longer wall-clock* for the same
  number of heartbeats). That can push wall-clock past
  `Harness::wait_for` timeouts and induce spurious failures. The
  scenario-classification step in the build order is the mitigation:
  CPU-bound scenarios get a serial pass, or their parallel slots
  reserve a full host core.
- **Harness wall-clock timeouts.** Every `Harness::wait_for(...)`
  uses host wall-clock. The harness assumes the guest will reach
  the assertion within that budget. Under multiplexing of CPU-bound
  scenarios this can fail for the reason above. We should review
  timeout budgets while implementing — a few are probably too
  tight (e.g. anything under 5s) and worth padding before the
  default `--jobs` flip.
- **Disk I/O.** Each QEMU writes its `.log` and reads its kernel
  ELF. On NVMe this is noise. On a CI runner with slow disk, it
  might shift timing. A/B catches this.
- **Heartbeat cadence assertions.** Some scenarios assert "two
  heartbeats within X seconds". Under CPU contention, X might
  marginally tighten and induce spurious failures. The two-prop
  test will surface this.

## Build order

1. **Wire `--jobs` plumbing.** Default `1`, range `1..=64`.
   No behaviour change at default. Useful to ship in isolation so
   the flag is available for manual experimentation.
2. **Per-worker `Aggregator` + merge.** Refactor `runner::run` so
   `Aggregator` is owned per-worker. Merge step at end. Still
   sequential. Adds a `Aggregator::merge(&mut self, other)`. Tested
   via existing host tests with a `--jobs 1` shape.
3. **Thread-scoped worker pool.** Inside `runner::run`, replace the
   sequential loop with a `thread::scope` + `Mutex<VecDeque>`
   work queue. Workers loop until empty / interrupted. Output
   prefixed with `[scenario]`.
4. **NDJSON writer lock.** Wrap `HistoryWriter` in a `Mutex`,
   workers acquire briefly per row.
5. **Classify scenarios as cheap / expensive.** Annotate each
   scenario with a `cpu_profile: CpuProfile { Wfi, Cpu }` (or
   similar). `Cpu` for the known offenders (`heap-oom`,
   `sched-yield-round-trips`, anything else that consumes a host
   core under guest steady-state); `Wfi` for everything else.
   Discoverable empirically: run `top`/`htop` against a 10-iteration
   parallel run, anything pinning a core is `Cpu`. The work-queue
   scheduler from step 3 then either runs `Cpu` scenarios in a
   separate serial pass, or reserves slot weight = 1 / `num_cpus`
   for each running `Cpu` scenario. Start with "serial pass" — it's
   the simplest correct thing.
6. **A/B harness (one shot, ad-hoc).** Run the two-prop test
   described above. Don't ship `--jobs > 1` as default until it
   passes.
7. **Default flip.** Change `--jobs` default to
   `min(2 × num_cpus, scenario_count)`. Document the flip in
   `posts/`. If A/B data showed CPU-bound scenarios shift even with
   the serial-pass mitigation, the right next step is per-scenario
   weighting rather than a smaller default.

Each step leaves the suite working. Steps 1-4 ship together as a
"opt-in --jobs N" PR; step 6 is a separate PR gated on step 5's data.

## Deferred: parallel iterations of one scenario

Today `--repeat 1000` runs 1000 sequential iterations of each
scenario. Parallelising *those* iterations would give the biggest
speedup for flake-hunting workflows.

The corrected mental model says this is mostly fine for wfi-bounded
scenarios — running 8 iterations of `heartbeat-cadence` in parallel
should give an ~8× wall-clock improvement, because the iterations
are each ~5 seconds of `wfi` waiting on `mtime` and the host CPU is
nearly idle. The risk profile is real but narrower than I sketched
in v1 of this plan:

- Per-iteration socket/log paths collide — needs an iteration
  discriminator (`-<iter>` suffix on the pid-based path).
- For **CPU-bound** scenarios, running N copies in parallel will
  shift flake characteristics for the same reason as scenario-level:
  contention pushes wall-clock past the harness's timeout. The
  fix is the same — use the scenario-classification to decide
  whether `--repeat N` of *this* scenario fans out or runs serial.

Don't do this until scenario-level parallelism (step 7) is shipped
and the A/B data shows it's measurement-clean. When we do, the
build order is short:

- Iteration discriminator in `socket_path_for` / `log_path_for`.
- Reuse the cheap/expensive classification from step 5.
- A new explicit A/B before defaulting on — the contention pattern
  is different (same scenario × N) and we can't assume scenario-level
  results carry over even though the mental model is the same.

## Open questions

- **Build cache.** With `--jobs > 1` and multiple `cargo xtask itest`
  invocations on different checkouts (e.g. a worktree per branch),
  do their kernel builds compete? Cargo's target-dir lock handles
  this, but worth confirming behaviour is sensible (one waits, both
  proceed afterward) rather than dropping a build failure.
- **Log dump on failure.** Today the runner dumps the tail of the
  log to terminal. Under parallelism, multiple simultaneous failures
  could spam interleaved log dumps. Buffer-and-flush-per-scenario at
  least for the FAILED branch? Decide while implementing step 3.
- **OTLP auto-push timing.** If we wire auto-push at end of run
  (separate plan), it runs once per `xtask itest` regardless of
  jobs count. No interaction.
