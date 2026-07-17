# itest shared-boot mode

Let a group of integration scenarios that share an identical kernel
boot run **either** (a) against one shared boot — each scenario reading
the same recorded frame stream through its own cursor — **or** (b) on a
fresh kernel per scenario (today's model). Choose the mode at run time.

Goal: cut dev-loop wall-clock by collapsing redundant boots, **without**
losing the per-scenario isolation, flake-rate baselines, and
failure-signature capture that the separate-boot model gives us.

## Status (2026-06-13)

- **Step 1–2 done** — `Harness` re-implemented on a `Recorder` (append-only
  frame buffer + condvar + per-handle cursor) instead of the consume-once
  mpsc channel. Behaviour-preserving; old fn signatures intact; 5 host
  unit tests (`cargo test -p xtask recorder_tests`) cover the cursor logic
  incl. two independent cursors replaying one buffer. Full QEMU suite green
  (run by the user).
- **Step 3 done** — `Scenario.workload: Option<&'static str>` +
  `on_workload` builder; `scenarios!` row grammar gains a `{"<workload>"}`
  clause (braced, not a `workload` keyword: a bare ident can't follow a
  `:path` matcher fragment). Catalog populated with every scenario's
  workload; guard comment on the nine `{"userspace"}` rows. Macro tests
  extended. Host-only; no scenario bodies touched yet.
- **Step 4 done (the runner inversion)** — itest-harness now delegates
  execution to a consumer executor and consumes structured reports:
  - `ScenarioReport { result, max_wait, capture, log_path }` (pub).
  - `RunnerConfig.run_group: Option<&dyn Fn(&[&Scenario]) ->
    Vec<ScenarioReport> + Send + Sync>`; the three thread-local hooks
    (`log_path_for`/`max_wait_for`/`capture_for`) + the `Hooks` struct are
    **deleted**. `process_one_scenario` runs a scenario via the executor
    (singleton group) and reads everything from the report.
  - `None` executor falls back to `(s.run)()` — keeps the crate's own
    runner tests unchanged. 157 itest-harness tests green.
  - xtask supplies the executor: calls `(s.run)()` (scenarios still spawn
    internally) and drains the harness thread-locals into the report.
    **Behaviour-identical** in separate mode; the runner is now inverted.
  - Host-only; QEMU behaviour unchanged but a confirming `--repeat 10` is
    wanted once the build is green (the user runs it).
- **Step 5 done (xtask Boot/View split + scenario migration), pending QEMU
  validation** — compiles + all host tests green; needs a `--repeat 10`
  confirmation once the build is unblocked.
  - xtask `Harness` split into `Boot` (owns QEMU + Recorder, kills on drop;
    `spawn(label, Option<workload>)`, `view()`, `log_path()`) and `View`
    (cursor + assertion state + `wait_for`/`assert_absent`/`name_of`/
    `timebase_hz`, plus `max_wait()`/`take_capture()` for the executor).
    The failure-capture + max-wait + log-path **thread-locals are deleted**
    — `View` carries them directly.
  - All 42 scenarios migrated to `fn(&mut View)` (the in-fn `spawn` lines
    removed; bodies otherwise untouched).
  - The catalog macro moved consumer-side as `catalog!` in xtask: it
    co-generates `SCENARIOS` (metadata; `run` is a never-called
    `unreached_run` placeholder) **and** `scenario_view_fn(name) ->
    fn(&mut View)` from the same rows (can't drift). The itest-harness
    `scenarios!` macro + its tests were removed (replaced by a builder
    test).
  - The executor spawns one `Boot` per group and runs each scenario's
    `fn(&mut View)` against a fresh `View`, building the report from
    `view.max_wait()` / `view.take_capture()` + `boot.log_path()`. Already
    group-shaped, so step 7 is just flipping the runner's grouping.
- **Steps 6 + 7 done and QEMU-validated (build unblocked).**
  - **Step 6 is moot** — each `View` builds its own incremental string
    table from cursor 0, so `pre_init_order`'s `name_of(id).is_none()`
    check works per-View with no special handling. It passes in shared mode.
  - **Step 7** — `RunnerConfig.shared` + `group_scenarios(scenarios,
    shared)` (separate → singletons; shared → group by `workload`,
    first-seen order). The runner's work unit is now a *group*: sequential
    and parallel paths both iterate groups; `run_parallel_batch` fans
    groups across workers; a group is Cpu-bound if any member is.
    `process_one_scenario` → `process_group` (+ `process_report` per
    member; output is now one atomic line per scenario). `--shared` CLI
    flag (default off). 3 new host tests (separate singletons, shared
    grouping, end-to-end group sizes via a fake executor).
  - **Validated on real QEMU:** full suite **43/43 pass in both modes**.
    Shared mode cuts CPU time **78.3s → 47.0s** (~40% fewer boots); wall
    time 44.5s → 41.2s (modest — the 19-scenario default-demo group is a
    serial long-pole on one worker).
- **Step 8 (docs) done** — `--shared` is documented in the README (the
  `cargo xtask itest` flag list). **Plan complete.**
- Optional, not done (and not blocking): wall-clock tuning — parallelise Views
  *within* a shared group; and/or the 10 Hz itest-timer from the suite audit
  (orthogonal, multiplies with this).

Non-goal: replacing the separate-boot model. Shared mode is an
additional, opt-in mode. The flake gate (`--repeat 10`) and baseline
updates keep running in separate mode.

## Why

41 scenarios today, but only ~16 *distinct* kernel boots. Two groups
dominate (audited 2026-06-13):

- **19** scenarios use `Harness::spawn` — the **identical default-demo
  kernel**. Boot / heartbeat / frame / heap / sched / smp-probe / ipi
  assertions all read frames from one boot.
- **9** scenarios use `spawn_with_workload(_, "userspace")` — the
  **identical `hello` run**. That single execution grants two caps,
  emits `telemetry=42`, opens `hello.work`, invokes a wrong-object
  handle (refused), invokes an ungranted handle (denied), yields, and
  exits. All nine scenarios' target frames come out of that one run.

So 28 of 41 scenarios re-boot QEMU to inspect disjoint frames from just
two kernels. In shared mode those 28 collapse to **2 boots**; the suite
goes from 41 boots to ~16.

The boot is the cost. Frame matching is a cheap scan of a decoded
`Vec<OwnedFrame>`. Today we pay the expensive part 41 times to do the
cheap part 41 times.

## The obstacle (why it isn't already possible)

`xtask/src/itest/harness.rs` is a **consume-once stream**. The reader
thread pushes decoded frames into an `mpsc::channel`; `wait_for` calls
`self.rx.recv_timeout` and each frame received is gone. The string table
(`StringId → name`) is built incrementally as frames drain.

Two scenarios can't share one `Harness`: the second starts wherever the
first drained the channel and misses the early frames (Hello,
ThreadRegisters) it needs.

Everything else is in our favour. Scenarios touch the kernel through a
tiny, uniform, **read-only** surface — `wait_for`, `assert_absent`,
`name_of`, `timebase_hz`. Nothing reaches into QEMU or mutates state.
That surface is the entire contract a cursor has to satisfy.

## Design: record-and-replay with per-scenario cursors

### Recorder (replaces the consume-once channel)

The reader thread appends every `OwnedFrame` to a shared, append-only
buffer instead of an mpsc channel:

```rust
struct Recorder {
    frames: Mutex<Vec<OwnedFrame>>,   // append-only; never drained
    grew: Condvar,                    // notified on each append + on close
    closed: AtomicBool,               // reader thread set this on EOF/disconnect
    strings: Mutex<StringTable>,      // built once over the full stream (monotonic)
    timebase_hz: AtomicU64,           // 0 until Hello; then the value
    // failure-capture bookkeeping (histogram, per-hart t, recent ring) moves here
}
```

The string table is **monotonic** — `StringRegister` only ever *adds*
ids, and ids are stable. So a complete table is correct for resolving
any historical frame's `name_id`. (One exception below.)

One Recorder owns one QEMU child + socket + reader thread + `Drop`
cleanup. That's the current `Harness`, minus the channel, plus the
buffer.

### View (the per-scenario cursor)

```rust
struct View<'r> {
    recorder: &'r Recorder,
    idx: usize,                       // this scenario's own position
    strings: StringTable,             // optional per-view incremental table (see exceptions)
    max_wait: (Duration, Duration),   // unchanged bookkeeping, now per-view
}
```

`View::wait_for(budget, pred)` scans `recorder.frames[idx..]`, advancing
`idx`; on reaching the end before a match it waits on `grew` up to the
deadline for more frames (or `closed`). `assert_absent`, `name_of`,
`timebase_hz` re-expressed against the same buffer. **The scenario-facing
API is identical** — only the receiver type changes (`&mut Harness` →
`&mut View`).

Independent `idx` per View means each scenario sees the whole stream
from position 0, regardless of what other Views consumed.

### How the two modes fall out

- **Separate (b)** — today's semantics. One Recorder/QEMU per scenario,
  one View at `idx 0`, run the fn. Per-scenario flake baseline +
  signature capture unchanged. **Flake-gate mode.**
- **Shared (a)** — group scenarios by declared workload string. One
  Recorder/QEMU per group; each scenario in the group gets its **own**
  View at `idx 0`; evaluate them sequentially on one worker (Vec scans
  are CPU-trivial). One boot per group: default-demo 19→1, userspace
  9→1. **Fast inner-loop / PR-check mode.**

The group's QEMU stays alive until the last View in the group returns
(or a group deadline trips), so liveness assertions ("kernel keeps
heartbeating after OOM") still hold.

The mode choice maps onto the two real use cases: shared for fast
feedback; separate for flake hunting and baselines, where isolation and
per-scenario flake rates actually matter. We keep the deflake machinery;
we just don't engage it in the fast mode.

## Step 4 architecture: invert where execution lives

The naive step-4 (make `Scenario.run` a `fn(&mut View)`) doesn't fit: `View`
is protocol/QEMU-specific so it must live in **xtask**, but `Scenario`
lives in the deliberately protocol-free **itest-harness**, whose generic
runner calls `(s.run)()` with no args and can't construct a `View`. The
shims/thread-locals that would bridge that are a symptom of a misplaced
seam. The well-factored fix is to **invert where execution lives**.

Today's tell: the runner reads `max_wait_for` / `capture_for` /
`log_path_for` *thread-locals* that the xtask harness stashes during
`(s.run)()`. That's action-at-a-distance because the scenario owns its
boot and the runner only gets to scrape the aftermath. Remove it.

**The split of responsibility:**

- The **runner** (itest-harness) owns *orchestration*: scenario selection,
  grouping, parallelism, `--repeat` aggregation, flake baselines, history,
  signature capture.
- The **consumer** (xtask) owns *execution of a group of scenarios against
  one subject*, returning structured reports.

Separate-vs-shared is then **not two code paths** — it's only *how the
runner groups before delegating*: separate → each scenario is a group of
one; shared → group by `workload`. The executor is identical for both; it
just runs "this group on one boot."

**itest-harness (generic, protocol-free):**

- `Scenario<P>` — metadata (`name`, `cpu_profile`, `tags`, `workload`)
  **plus an opaque payload `P`** the runner carries but never inspects.
  `P` defaults to `fn() -> Result<(), String>` (keeps the crate's own
  runner tests trivial); xtask sets `P = fn(&mut View) -> Result<(), String>`.
- `ScenarioReport { result, max_wait, capture, log_path }` — returned by
  execution, **replacing the three thread-local hooks entirely**.
- A consumer executor in `RunnerConfig`:
  `run_group: &dyn Fn(&[&Scenario<P>]) -> Vec<ScenarioReport>`.
  The runner groups, calls `run_group` per group, aggregates the reports
  exactly as it aggregates results today.

**xtask (owns everything frame/QEMU, cohesively):**

- `Boot` — owns the QEMU child + socket + `Arc<Recorder>`; kills on drop.
- `Recorder` — the append-only frame buffer (already built, step 1–2).
- `View` — cursor + per-scenario assertion state + frame-specific helpers
  (`wait_for`, `assert_absent`, `name_of`, `timebase_hz`); the prefactored
  `Harness` *minus* QEMU ownership.
- `run_group(group)` — spawn **one** `Boot`, run each scenario's
  `fn(&mut View)` against a fresh `View`, collect `ScenarioReport`s, drop
  the `Boot`. Mode-agnostic: the runner decides whether the group has 1 or
  9 members.

**What this buys:**

- Separate and shared collapse to one executor; the only difference is the
  runner's grouping (generic — group by `workload`).
- Scenarios become pure `fn(&mut View)` assertions: composable, no
  in-fn `spawn`, no thread-locals.
- The `max_wait_for` / `capture_for` / `log_path_for` hooks **disappear**,
  replaced by `ScenarioReport` fields — no action-at-a-distance.
- The crate boundary becomes honest: itest-harness orchestrates *opaque*
  scenarios; xtask owns the subject + assertions. This is the "consumers
  plug in their own Subject" design the crate's own docs gesture at.

**Cost (accepted):** `Scenario<P>` ripples generics through the runner and
`RunnerConfig`; the runner's per-scenario path is rewritten from
`(s.run)()` + hook-scraping to `run_group(group) -> reports`; the harness's
own runner tests gain a trivial `P = fn()` executor; xtask's `harness.rs`
splits into `Boot` + `View` + the executor; all 41 scenarios migrate to
`fn(&mut View)` and the catalog/macro carry the payload.

## Declaring the boot in the catalog

Today the workload is a string buried inside each scenario fn
(`Harness::spawn_with_workload("urefuse", "userspace")`). To group by
boot, the workload must be **declared on the row**, and the fn must stop
spawning.

Extend the `scenarios!` row grammar with an optional workload token:

```
<profile> "<name>" <fn> [tags]? (workload "<name>")? ;
```

```rust
wfi "userspace-emits-telemetry" scenarios::userspace_emits_telemetry [userspace] workload "userspace";
wfi "boot-reaches-heartbeat"    scenarios::boot_reaches_heartbeat     [boot];          // no workload = default demo
```

`Scenario` gains `workload: Option<&'static str>`. The grouping key in
shared mode is exactly this field; `None` is its own group (default
demo). Singletons (storms, OOMs, tlb-shootdown) become groups of 1 —
shared mode boots them once, same as separate, no change.

Note: a scenario's label (the first arg to `spawn_with_workload`, e.g.
`"urefuse"`) was only ever a socket/log filename hint. It moves into the
runner, derived from the scenario name.

## Scenario signature migration

`fn() -> Result<(), String>` becomes `fn(&mut View) -> Result<(),
String>`. Per scenario this is mechanical:

1. Delete the `let mut h = Harness::spawn[_with_workload](...)?;` line.
2. Rename the receiver: `h.wait_for(...)` → `view.wait_for(...)` (and
   `h.name_of` / `h.timebase_hz` / `h.assert_absent`).

41 functions, rote. The workload string each used moves to its catalog
row (previous section). *Who* calls these `fn(&mut View)`s — the
`run_group` executor that spawns the `Boot` and hands each a fresh `View`
— is covered in "Step 4 architecture"; the scenario bodies themselves no
longer spawn anything.

## Exceptions (the actual risk surface)

1. **Same-workload-only sharing.** Only scenarios with the same
   `workload` field can share a boot. Enforced by the grouping key; no
   judgement needed.

2. **Superset requirement.** A shared workload must emit a superset of
   every grouped scenario's frames. True for default-demo and
   `userspace` today. Add a guard comment on the userspace group so a
   future scenario that needs a *different* `hello` program is forced to
   a new workload name rather than silently sharing.

3. **`pre_init_order` needs the incremental table.** It asserts
   `name_of(id).is_none()` to catch out-of-order registration — a
   complete table makes that vacuous. Fix: give each View an *optional*
   per-view incremental `StringTable` it builds as `idx` advances (the
   `strings` field above), used by this scenario instead of the
   Recorder's complete one. Cheap, and keeps the assertion meaningful in
   both modes. Alternatively tag it `separate-only` (below) — but the
   per-view table is the clean answer and most scenarios just ignore it.

4. **`separate-only` escape hatch.** A `Scenario` flag (or a `[…]` tag
   convention) marking scenarios that must not share a boot. Shared mode
   runs them as groups of 1. Reserved for anything order/incremental
   sensitive we don't want to adapt. Expected users: none after the
   per-view table; keep it as a safety valve.

5. **Infra-failure attribution coarsens in shared mode.** A QEMU death
   mid-stream fails every View in the group at once. That is correct (it
   is one failure), but it's why per-scenario flake baselines belong in
   separate mode. Document it; don't update baselines from shared runs.

## Runner + CLI

- `--shared` flag on `cargo xtask itest` (default off → separate, today's
  gate semantics preserved). Composes with `--tag` (`--tag userspace
  --shared` = the nine userspace assertions off one boot) and `--skip`.
- The runner's grouping is the *only* mode-dependent code: separate →
  each scenario is a singleton group; shared → partition `to_run` by
  `workload`. Both then call the same `run_group` executor (per "Step 4
  architecture") and aggregate the returned `ScenarioReport`s. Groups fan
  across the existing worker pool — the unit of parallelism becomes the
  group; within a group the executor runs Views sequentially (Vec scans
  are CPU-trivial).
- The Cpu/Wfi profile partition still applies at the group level: a group
  is Cpu-bound if any member is.
- Baseline writes + `--update-baseline` refuse (warn) in shared mode —
  flake rates from shared runs aren't comparable to the per-scenario
  baseline.

## Incremental delivery (TDD, each step green)

The harness has host unit tests (`cargo test -p itest-harness`) and the
scenarios run under QEMU. Recorder/View logic is host-testable without
QEMU by feeding a synthetic `Vec<OwnedFrame>`.

1. **Recorder + View, host-tested in isolation.** New module; feed it a
   hand-built frame vec + a "closed" signal. Test: two Views over one
   buffer each match independently from idx 0; `wait_for` blocks then
   wakes on append; `assert_absent` clean-elapses; timebase/name_of
   resolve. No QEMU, no scenario changes yet. (RED tests first.)
2. **Re-implement `Harness` on the Recorder, keep the old fn signature.**
   `Harness::spawn*` builds a Recorder + a single View internally and
   delegates `wait_for`/etc. to it. All 41 scenarios and the runner are
   untouched; the full QEMU suite must stay green. This proves the
   record-and-replay path is behaviour-preserving before any migration.
   Validate with `--repeat 10` (the commit gate).
3. **Add `workload` to the `scenarios!` row grammar + `Scenario`.**
   Host-test the macro expansion (extend the existing `scenarios_macro_*`
   tests). Catalog still compiles; workload field unused so far.
4. **Invert execution in itest-harness (host-tested, no QEMU).** This is
   the core of "Step 4 architecture". Sub-steps, each green:
   a. `Scenario<P = fn() -> Result<…>>` generic + `ScenarioReport`. The
      crate's own runner tests gain a trivial `P = fn()` executor and
      assert on returned reports instead of thread-locals.
   b. Replace the runner's `(s.run)()` + `max_wait_for`/`capture_for`/
      `log_path_for` hooks with a `run_group: Fn(&[&Scenario<P>]) ->
      Vec<ScenarioReport>` executor + grouping (separate = singletons).
      Delete the three thread-local hooks. All host-testable with a fake
      executor.
5. **Split xtask `Harness` → `Boot` + `View`; add the `run_group`
   executor.** `Boot` owns QEMU + Recorder; `View` is the cursor +
   assertions. Executor spawns one Boot per group, runs each payload
   against a fresh View, returns reports. Migrate all 41 scenarios to
   `fn(&mut View)` and the catalog/macro to carry the `fn(&mut View)`
   payload. Separate mode must stay behaviour-identical — validate with
   `--repeat 10` (the commit gate). This is the step that needs QEMU.
6. **Per-view incremental table for `pre_init_order`** (exception 3).
7. **Runner: `--shared` + grouping.** Default off — flip the grouping from
   singletons to group-by-`workload`. The executor is unchanged. Host-test
   the grouping; then `cargo xtask itest --tag userspace --shared` boots
   once for all nine; compare wall-clock; full-suite `--shared` green.
8. **Docs:** README flag + a short `docs/` note; a guard comment on the
   userspace group (exception 2).

Land 1–3 first (done). Step 4 (a/b) is host-only and retires the runner
re-architecture with zero QEMU risk. Step 5 is the QEMU-validated one and
should be done against a green build. 6–8 are additive.

## Risks / open questions

- **Shared-group deadline.** One group, many Views with different stream
  appetites (early frames vs the 200-sample threshold). The group QEMU
  must outlive the slowest View. Use `max(per-view budgets)` as the group
  deadline, or keep QEMU alive until all Views return. Lean: keep alive
  until all return or a hard group cap trips.
- **A shared View that wedges** shouldn't hang the group. Each
  `wait_for` keeps its own budget, so a stuck View fails on its deadline
  and the group proceeds — same as today, per-call.
- **Memory.** A full boot's frame stream buffered as `OwnedFrame`s.
  Bounded by run length; a default-demo boot is well under a few MB.
  Trivial. (The storm workloads emit the most; they're singletons.)
- **Does shared mode ever hide a bug separate mode would catch?** Yes in
  principle: a bug that only manifests under per-scenario *boot* timing
  (e.g. a boot-ordering flake) won't reproduce when 9 assertions share
  one boot. That's exactly why the flake gate stays in separate mode.
  Shared mode is for fast confirmation, not for flake hunting.

## Payoff

- Boots: 41 → ~16 in shared mode (default-demo 19→1, userspace 9→1).
- Wall-clock dominated by the slowest group's boot+drain, groups in
  parallel — a large cut for the inner loop and PR checks.
- Orthogonal to (and stacks with) the **10 Hz itest-timer** idea from the
  suite audit, which cuts each *boot's* time-to-threshold ~10×. Shared
  mode cuts boot *count*; the faster timer cuts boot *duration*.
- Separate mode unchanged: the flake gate and baselines keep their
  per-scenario isolation.

## Effort

Medium, less than it looks. Steps 1–2 (the Recorder/View + behaviour-
preserving Harness re-impl) are the conceptual core and are ~100 lines +
host tests. Step 4 is rote across 41 fns. Steps 3/6 are small. The novel
part — cursors over a shared append-only buffer — is a well-trodden
pattern. A focused day, not a weekend.
