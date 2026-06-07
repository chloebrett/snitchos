# Extracting the integration-test harness into its own crate

## why

`xtask/src/itest/` mixes two concerns:

- **Generic runner mechanics** — `--repeat` aggregation, log-dump on failure, kill-stale, build-once, scenario lifecycle. None of this knows about QEMU or SnitchOS.
- **SnitchOS-specific glue** — QEMU command builder, virtio-console socket reader, postcard `Frame` decoder, matchers, scenario implementations.

The mix made the deflake-bisection session messier than it needed to be: harness changes (build-once) and kernel changes (deflake patches) landed in adjacent files; statistical tooling proposals (baseline file, Wilson CI, Fisher's exact) have nowhere clean to live; the harness has no host-side tests because there's no test boundary that excludes the QEMU process.

Same logic as `kernel` vs `kernel-core`: separate the platform-pure logic so it's host-testable, then have the platform-specific code consume it. We're going to want this even if no one else ever uses the harness crate — the boundary is the discipline.

## crate boundary

New workspace member: `itest-harness/`. Host-buildable, no platform dependencies, no QEMU mention anywhere in its source. Wherever it touches "the thing under test," it goes through a trait.

### what's in

- the `--repeat N` runner loop, per-scenario aggregation, flake summary
- the failure log-dump (last 80 lines from a path the consumer provides)
- the kill-stale + one-shot-build hooks (called by the runner, implemented by the consumer)
- the baseline file (`.itest-baseline.json` schema, read/write, regression verdict)
- Wilson-score CI, Fisher's exact, two-proportion z
- `--fail-fast=K`
- the counterfactual-matrix runner (parameterised by a list of `Subject` configs)
- timing instrumentation (max-wait per scenario, budget surfacing)
- the CLI args struct (clap-friendly, exported for consumers to flatten into their own CLI)

### what's out

- anything QEMU-specific (lives in `xtask` as a `QemuSubject` impl of the harness's `Subject` trait)
- the postcard `Frame` decoder (lives in `protocol`)
- matchers like `is_hello`, `is_metric_named`, etc. — these are SnitchOS-scenario helpers, not harness primitives
- the scenario implementations themselves

## core abstractions

```rust
/// What's under test. The harness handles process lifecycle; you
/// provide the launch command and event-stream decoder.
pub trait Subject: Sized + Send {
    type Event: Send;
    type Error: core::fmt::Display;

    /// Spawn the subject. Returns a launched handle; Drop should kill
    /// the process.
    fn launch(&mut self) -> Result<LaunchedSubject<Self>, Self::Error>;

    /// Human-readable label for logs.
    fn label(&self) -> &str;
}

/// Open handle to a running subject. The harness owns the channel
/// between the subject and the test thread.
pub struct LaunchedSubject<S: Subject> { /* opaque */ }

impl<S: Subject> LaunchedSubject<S> {
    /// Block up to `budget` for the next event. Returns `None` on
    /// timeout or disconnect.
    pub fn next_event(&mut self, budget: Duration) -> Option<S::Event>;

    /// Recent events for failure dumps. Bounded.
    pub fn recent(&self) -> &[S::Event];

    /// Did the subject disconnect cleanly?
    pub fn disconnected(&self) -> bool;
}

/// One scenario.
pub trait Scenario {
    type Subject: Subject;
    fn name(&self) -> &'static str;
    fn run(&self, subject: &mut LaunchedSubject<Self::Subject>)
        -> Result<(), String>;
}

/// Runner configuration. Hooks default to no-ops; consumers wire what
/// they need.
pub struct RunnerConfig<'a> {
    pub kill_stale: Option<&'a dyn Fn()>,
    pub one_shot_build: Option<&'a dyn Fn() -> Result<(), String>>,
    pub log_path_for: &'a dyn Fn(&str) -> PathBuf,
    pub baseline_file: Option<PathBuf>,
}
```

## migration steps

Each step keeps the suite green. Each step is one commit. No big-bang.

### 1. add the workspace member

- `itest-harness/Cargo.toml` (empty deps, `edition = "2024"`)
- `itest-harness/src/lib.rs` with a single `pub use` re-export so the crate compiles
- Add `"itest-harness"` to root `Cargo.toml` `members`
- Verify `cargo xtask build` still works

### 2. move pure-Rust mechanics first

These don't need traits yet — just types that don't depend on QEMU:

- the per-scenario aggregation logic (`BTreeMap<&str, u32>`, run-totals vector)
- the `--repeat` loop structure
- log-dump (takes a `&Path`)

These move as concrete types initially. The trait shape will land in step 4.

### 3. move statistical machinery + baseline file

- Wilson-score CI
- Fisher's exact (or two-proportion z — pick one, document the trade-off)
- `.itest-baseline.json` schema, read/write
- regression verdict computation

This work doesn't exist yet — write it into the new crate from the start. Host tests for the math go in `itest-harness/tests/`.

### 4. define `Subject` + `Scenario` traits, refactor existing harness to implement them

This is the biggest step. The existing `Harness` becomes `QemuSubject` in xtask, implementing `Subject<Event = OwnedFrame>`. The `Scenario` struct in `xtask/src/itest.rs` becomes an `impl Scenario for ScnHeartbeatCadence`-style each.

Decisions to lock in:

- Should `next_event` be `Option<Event>` or `Result<Event, RecvError>` with a `Timeout` variant? `Option` is simpler; `Result` is more honest about timeout vs disconnect.
- Should the event-stream be a channel (current) or an async stream? Channel for now — no async runtime in xtask. Note this in the crate-level doc.
- How are recent-events surfaced for failure dumps? Bounded `VecDeque<Event>` accessed via `recent(&self) -> &[Event]`. Cap at 8 (current value).

### 5. move the runner

- The `for run_idx in 0..runs { for scenario in scenarios { ... } }` loop body, now parameterised by `&dyn Scenario<Subject = S>` for some `S: Subject`.
- The `RunnerConfig` hooks get plumbed in.
- xtask's `itest::run` shrinks to: build a `RunnerConfig` with the QEMU-specific closures, instantiate scenarios, call `itest_harness::run`.

### 6. audit `pub` vs `pub(crate)`

Once the suite is green again, walk the crate's public surface. Start conservative — anything that doesn't need to be `pub` becomes `pub(crate)`. Easier to expand than retract later.

### 7. land the proposed tooling

Now that the boundary exists, the proposals in `concurrency-debug-tooling.md` go into the harness crate:

- inline per-scenario rates with CI framing
- `--fail-fast=K`
- counterfactual-matrix runner
- baseline-comparison verdict on every run

Each becomes a small PR against `itest-harness` with host tests, then xtask gets a flag for it.

## decisions to surface as we go

Track these in this doc as they get resolved:

- [ ] `next_event` return type: `Option<Event>` vs `Result<Event, RecvError>`
- [ ] timeout-vs-disconnect distinction: handled by `LaunchedSubject::disconnected()` or baked into the return type
- [ ] async or not: assume not for now; revisit if/when xtask grows an async dep for other reasons
- [ ] should `Scenario` be a trait or a struct holding `fn() -> Result<…>`? trait is more flexible; struct is what we have. Keep as struct for migration, revisit
- [ ] do we expose `RunnerConfig` hooks as `&dyn Fn` or as a trait with named methods? `&dyn Fn` is lighter; trait is more discoverable
- [ ] CLI arg parsing: does `itest-harness` ship a clap struct, or just types the consumer wires up? clap struct, behind a `clap` feature flag

## host-test targets

Once the crate exists, write host tests for:

- Wilson-score CI on hand-computed cases
- Fisher's exact on hand-computed cases (compare against R/scipy ground truth)
- Baseline-file read/write round-trip
- Aggregation logic: feed scripted pass/fail sequences, assert flake table
- Runner loop with a `FakeSubject` that emits a scripted event sequence

The last one is the high-value one. It lets us test "if the subject emits these events, does scenario X pass or fail?" without QEMU in the loop. The deflake session would have benefited from being able to unit-test scenario assertions against a known frame sequence.

## success criteria

- `cargo xtask itest --repeat N` produces identical output to today's version (modulo new rate framing).
- `cargo test -p itest-harness` runs in <2 seconds, covers the aggregation + statistics with property tests where reasonable.
- The xtask side of `itest` is < 300 LOC (currently ~500), with the residual being SnitchOS-specific glue only.
- Adding a new feature (e.g. `--fail-fast=K`) requires editing only `itest-harness/` plus a one-line CLI flag wire-up in xtask.
- The baseline file lives in the repo root and updates correctly via `--update-baseline`.

## non-goals

- Publishing to crates.io. The crate is internal; can ship later if a second consumer materializes.
- Generalising beyond stdio/subprocess + event-channel subjects. Async runtimes, fuzzing harnesses, distributed tests — not in scope.
- Re-designing the matchers DSL. Matchers stay scenario-side in xtask.
- A second `Subject` impl (e.g. a `MockSubject` for production use). The fake subject is for tests only.

## risks

- **Trait shape lock-in too early.** Mitigation: keep types `pub(crate)` aggressively in step 6. The first round of "make it work" can be ugly; the second round (when feature work starts landing) is where the shape gets locked.
- **Migration stalls halfway.** The `xtask/src/itest/` and `itest-harness/` directories both being live at once is fine for a day, ugly for a week. Each step's commit needs to leave the suite green so we can ship partial migrations to main.
- **Unexpected platform coupling.** If something in `xtask/src/itest/harness.rs` turns out to be QEMU-aware in a non-obvious way (e.g. specific exit-code interpretation), surface it during migration rather than papering over.

## prior art

Audited the space — no drop-in replacement exists, but several crates
have conventions worth matching.

### closest architectural cousins (worth studying)

- **`testcontainers-rs`** — Docker container lifecycle for integration
  tests. Same shape: spawn an external thing, hold a handle, Drop kills
  it, expose readiness/logs. Docker-only, no flake math. Borrow: trait
  shape, lifecycle method names (`start()` → handle, `log()` accessor).
- **`defmt-test` + `probe-run`** — embedded test runner: launch binary,
  capture event stream over a serial-ish channel, parse to test
  results. Borrow: event-protocol abstraction, the "consumer owns the
  decoder" split.
- **`insta`** — snapshot testing. Not a process harness, but the
  **file-based-baseline workflow** is the most polished prior art for
  `.itest-baseline.json`. Borrow: file commits to repo, updates are
  explicit human acts (`cargo xtask itest --baseline-review`), warn if
  HEAD ≠ baseline commit.
- **`proptest`** — regression seed files (`proptest-regressions/`).
  Borrow: when a specific run fails, save enough info to replay
  (kernel build hash, QEMU args, scenario name). Adjacent to baseline
  file conceptually.
- **`nextest`** — Rust test runner with polished flake UX (`--retries`,
  slow-test detection, partitions). Borrow: per-scenario terminal lines
  with running rate, slow-test highlighting.

### considered and rejected

- **`escargot`** — build-once for cargo binaries. We build once per
  `--repeat N`, not per `#[test]`, so direct `cargo build` is fine and
  saves a dep.
- **`assert_cmd`** / **`rexpect`** — basic subprocess + assertion. No
  aggregation, no flake handling. Insufficient.
- **`testcontainers-rs` as base** — Docker-only. The Subject trait
  needs to abstract over "any subprocess" not just containers.
- **`nextest` as base** — it's a *test runner* for `#[test]` functions,
  not a library for harnessing external subjects. The architecture
  fights us.

### what's missing across the whole ecosystem

- per-scenario flake-rate tracking with statistical framing
- versioned baseline file with two-sample regression verdict
- counterfactual matrix runner

These don't exist in a public crate. The component pieces do; the
integration is novel. So the crate gets built — but its conventions
should feel familiar to anyone who's used `insta` (for the baseline
file) or `proptest` (for the regression seed pattern).

## sequencing relative to other work

Worth doing **before** the deflake tooling features land (per-scenario rates, fail-fast, baseline file). Those features want a clean home. Doing the extraction first means each tooling feature is one clean PR against `itest-harness` rather than a patchwork edit across `xtask`.

Order:

1. This extraction (steps 1–6).
2. The tooling features in `concurrency-debug-tooling.md` (each becomes a step-7 sub-task).
3. The deflake-residual hunt itself, now with the better tooling.
