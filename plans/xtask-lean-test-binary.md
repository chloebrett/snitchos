# Plan: Split xtask so the test path is snemu-free

**Branch**: main (no feature branch, per repo convention)
**Status**: Step 1a committed. Steps 1b + 2 DONE (uncommitted) — see result below.

## RESULT — Steps 1b + 2 (2026-07-19)

Done together (the diagram coupling below made the dep removal fall out naturally):

- **`xtask-itest` crate created**; the runner half (`itest.rs` + submodules),
  the `snemu` group (`run_snemu`/`snemu_boot`/`SnemuCmd`), baseline, itest-show,
  and their `cli_surface_tests`/`retired_command_tests` moved into it. Lean `xtask`
  forwards `itest`/`snemu`/`baseline`/`itest-show`/`diagram` via a raw-argv
  `delegate_itest("<sub>", &args)` → `cargo run -p xtask-itest -- <sub> …`.
- **Diagram had to move too (unplanned).** `diagram_cmd` reads `itest::SCENARIOS`
  (now in xtask-itest) *and* folds snemu frames via `snemu_diff` — so it's
  fundamentally heavy. Moved `diagram_cmd.rs` to xtask-itest; lean `diagram`
  delegates. The generated-diagram **drift check moved out of lean `xtask test`**
  into an xtask-itest `#[test]` (`diagram_drift_tests`), which runs in the nextest
  phase so the lean tool never links snemu. (This is the "diagram nuance" the plan
  flagged, resolved.)
- **Step 2 folded in:** with all snemu-touching code gone from lean, its `snemu` /
  `xtask-snemu` / `itest-harness` / `diagram` (+ now-unused `magnitude` / `protocol`
  / `snitchos-abi` / `fs-proto`) deps were removed from `xtask/Cargo.toml`.
  `cargo tree -p xtask | grep -c snemu` = **0**.

**Verified:** `touch snemu/src/lib.rs && cargo build -p xtask` → no snemu compile in
the tool; full gate `cargo xtask test` = 2098 tests, **1984 pass, only the
pre-existing `mutant_plan_tests` trip-wire fails** (user-owned; it now also lists
`xtask-itest`); `cargo xtask itest boot-reaches-heartbeat` and `cargo xtask diagram
deps --check` both work via delegation; `cargo xtask links` clean; clippy no errors.
`deps.md` regenerated (new `xtask-itest` node). Trade-off accepted: standalone
`cargo xtask diagram …` now builds xtask-itest (snemu); the frequent path (drift via
`test`) stays snemu-free.

Left for Step 3: docs (CLAUDE.md xtask-layout note). Not yet done.

## Goal

Stop `cargo xtask test` (and every other non-emulator command) from compiling
`snemu` into the xtask *tool* binary, so editing `snemu` and running `x test`
compiles snemu **once** (its own test-profile build inside nextest) instead of twice.

## Background — why two compiles today

`xtask` is a single binary that statically links `snemu` (directly, and via
`xtask-snemu`) because the itest harness drives a `snemu::Machine` in-process. So
`touch snemu/src/lib.rs && cargo xtask test` does:

1. **Build the tool** (`dev` profile) — recompiles snemu → xtask-snemu → xtask just to
   produce the `xtask` binary. ~27s. **Pure overhead for `test`**, which never runs the
   emulator.
2. **Run nextest** (`test` profile) — recompiles snemu with the test harness to run
   snemu's own unit tests. ~31s. **Inherent** — we want those tests.

Only (1) is waste. Cargo compiles a crate's declared deps regardless of which
subcommand runs, so the only way to keep snemu out of the tool is to keep it out of the
tool *crate*. That means a **binary split**, not a feature flag (feature toggling one
crate would rebuild the xtask binary on every `test`↔`itest` alternation — worse
thrash). "snemu as a binary artifact dep" is also rejected: `xtask-snemu`/`itest`
use snemu's *library* API richly (Machine, `state_hash`, snapshot-tree, folds,
profiling); a subprocess boundary would be a large rewrite that loses that and saves
nothing for `itest` (which must compile snemu either way).

## The split

- **Lean `xtask`** (no snemu): `test`, `build`, `boot`, `collect`, `reader`, `stack`,
  `clippy`, `links`, `audit`, `loc`, `measure`, `snip`, and the **static** diagram drift
  check that `test` performs (via the `diagram` lib + cargo metadata — no snemu boot).
- **Heavy `xtask-itest`** (links snemu): `itest` (+ `--engine qemu`/`--scramble`/`--opt`
  …), the `snemu` command group (`diff`/`bench`/`profile`), and the **telemetry**
  diagram targets (`caps`/`trace`/`switches`, which fold frames from a snemu boot).
- **Routing:** the `cargo xtask = run --package xtask --` alias is unchanged. Lean
  `xtask` recognises the heavy subcommands and forwards raw argv via
  `cargo run -p xtask-itest -- …`. `x`/`cargo xtask itest …` keep working verbatim.

**Diagram nuance (decide during Step 2):** the `test` drift check needs only the
`diagram` *library* on static projections (already the case — xtask boots snemu and
hands frames in; the lib itself doesn't depend on snemu). So the lean binary keeps the
drift check by calling the lib directly, while the `diagram` *command's* telemetry
targets move to `xtask-itest`. Confirm `diagram` has no `snemu` dep before relying on
this.

## Acceptance Criteria

- [ ] `touch snemu/src/lib.rs && cargo xtask test` shows **`Compiling snemu` exactly
      once** (inside the nextest build), with **no** snemu compile in a preceding
      tool-build phase.
- [ ] Every subcommand behaves identically: `cargo xtask itest [scenario]`,
      `--engine qemu`, `--scramble`, `--opt …`, `snemu diff|bench|profile`,
      `diagram <target>`, `test`, `build`, `boot`, `clippy`, `links`.
- [ ] The gate passes end to end: `cargo xtask test && cargo xtask itest && cargo xtask
      itest --scramble`.
- [ ] CLI-surface parity: every argv that parsed before still parses (the moved
      `cli_surface_tests` stay green in their new crate).
- [ ] `x test`, `x itest`, `x boot` (the shell alias) all still work.

## `itest.rs` is two disjoint halves (verified) — do NOT move it wholesale

The original Step 1 said "move `itest.rs` wholesale into `xtask-itest`." That is **wrong**
and would drag `test`/`clippy`/`mutants` into the snemu-linked binary. `xtask/src/itest.rs`
holds two halves that do **not** cross-reference (grep-verified):

- **Lean gate machinery** (implements `test`/`clippy`/`mutants`, no snemu, no
  itest-harness): `run_unit_tests`, `workspace_members`, `unit_test_plan`,
  `riscv_only_plan`, `NOT_HOST_TESTED`, `EXTRA_TEST_ARGS`, `check_rustdoc`,
  `run_cargo_test`, `cargo_metadata_json`, `workspace_manifests`, the in-flight lints
  WIP (`LINTS_EXEMPT`, `opts_into_workspace_lints`, `lints_optin_gaps`), and the
  `unit_test_plan_tests`/`lints_policy_tests`/`riscv_only_plan_tests` modules. **Stays
  in `xtask`** (extract to a new `xtask/src/plan.rs`).
- **Heavy runner** (the `itest`/`snemu` commands): the 7 submodules
  (`harness`/`scenarios`/`matchers`/`schedule`/`snapshot_tree`/`snemu_audit`) + `baseline`,
  `run`/`RunConfig`/`set_capture_level`/`show`/`latest_run_dir`/`find_capture`/
  `try_auto_push`/`unreached_run`/`install_ctrlc_handler`, and `qemu_available`/
  `detect_stale_qemus` (called only from `run`). Heavy only via `snemu_audit`'s direct
  `snemu` use + the `snemu` group. **Moves to `xtask-itest`.** (itest-harness itself
  does NOT link snemu.)

No shared lib crate is needed — the halves are independent.

## Steps

Each step leaves the gate green (except the pre-existing mutant trip-wire, owned by the
user). This is a build-structure refactor, so the headline acceptance (Step 2) is a
**measured** build-graph outcome; CLI parity and dispatch *are* unit-tested.

> **Sequencing caveat:** the lean half contains the user's uncommitted lints WIP. Do
> Step 1a only after that WIP is committed/settled, or explicitly accept relocating it.

### Step 1a: Extract the lean `plan` module out of `itest.rs` (lands in `xtask`, green)

**Acceptance criteria**: the gate-machinery items above move from `itest.rs` to
`xtask/src/plan.rs` (or similar); `test`/`clippy`/`mutants` call `plan::…` instead of
`itest::…`; nothing changes behaviourally; gate green. Pure intra-crate refactor — no new
crate, no snemu movement yet.
**RED/GREEN**: the moved `unit_test_plan_tests`/`lints_policy_tests`/`riscv_only_plan_tests`
compile and pass from `plan.rs`; `cargo xtask test` still runs the full suite.
**Done when**: `itest.rs` holds only the runner half; gate green; approved.

### Step 1b: Move the runner half + `snemu` group into `xtask-itest`; delegate

**Acceptance criteria**: `cargo run -p xtask-itest -- itest boot-reaches-heartbeat` runs
the scenario; `cargo xtask itest …` / `cargo xtask snemu …` forward to it and behave
identically. `xtask` keeps its snemu deps for now (no win yet). Baseline / itest-show
commands (runner-adjacent, no snemu) move with the runner. **Present the final command
boundary + delegation mechanism and confirm before writing code.**
**RED**: the itest/snemu `cli_surface_tests` move to `xtask-itest` and pass there; add a
lean-side test that the `itest`/`snemu` arms construct a `cargo run -p xtask-itest -- …`
command with argv forwarded unchanged (assert on the injected `Command`, don't execute).
**GREEN**: new `xtask-itest` binary crate (deps: snemu, xtask-snemu, itest-harness,
xtask-qemu, protocol[std], snitchos-abi, fs-proto, magnitude, serde, serde_json, ctrlc);
lean forwarding shim; register in workspace; update xtask's committed derived-lists to
include `xtask-itest` so `unit_test_plan_tests::the_committed_lists_match_the_workspace`
stays green.
**MUTATE / KILL MUTANTS**: `mutation-testing` on the forwarding/dispatch logic.
**Done when**: both entry points run scenarios identically; gate green; approved.

### Step 2: Drop `snemu`/`xtask-snemu`/`itest-harness` from the lean `xtask` crate

**Acceptance criteria**: `touch snemu/src/lib.rs && cargo xtask test` compiles snemu
exactly once (the win). Resolve the diagram nuance: keep the static drift check in lean
`xtask` via the `diagram` lib; move telemetry diagram targets to `xtask-itest`.
**RED**: add/keep a check that lean `xtask` no longer references snemu types (a grep
gate in CI, or simply that removing the deps still compiles). The drift-check path must
stay covered — `cargo xtask test` still fails on a deliberately drifted generated
diagram.
**GREEN**: remove the three deps + the now-dead snemu-using arms from
`xtask/Cargo.toml`/`main.rs`; drop any dep that only itest used (audit `fs-proto`,
`snitchos-abi`, `protocol` usage in the lean crate and remove if unused).
**MUTATE / KILL MUTANTS**: n/a for dep removal; ensure the drift-check test still kills
its mutant (drift detected).
**REFACTOR**: tidy `main.rs` now that the heavy arms are gone.
**Done when**: the one-compile acceptance holds and the full gate is green.

### Step 3: Docs + alias/dispatch polish

**Acceptance criteria**: CLAUDE.md's testing/xtask sections describe the two binaries
and the routing; README `x …` examples unchanged and correct; `cargo xtask links`
passes.
**RED**: `cargo xtask links` (and the doc-link check inside `cargo xtask test`) fail if
any moved/renamed path breaks a relative `.md` link.
**GREEN**: update CLAUDE.md (the "x test compiles snemu once" note; the xtask
crate-layout paragraph), README, and confirm the `cargo xtask` alias needs no change.
**Done when**: docs match reality, all links resolve, gate green.

## Pre-PR Quality Gate

1. `cargo xtask test && cargo xtask itest && cargo xtask itest --scramble` green.
2. Mutation testing on the new dispatch/forwarding logic.
3. `cargo xtask clippy` clean.
4. `cargo xtask links` clean.
5. Manual: `touch snemu/src/lib.rs && time cargo xtask test` shows a single snemu
   compile (record before/after wall-clock in the PR description).

## Rejected alternatives

- **snemu as a binary-artifact dependency** — loses the library integration
  (`state_hash`, snapshot-tree, folds, profiling); zero benefit for `itest`.
- **Cargo feature flag on the single binary** — feature-set flips between `test` and
  `itest` would rebuild the xtask binary each alternation: more thrash, not less.

---
*On completion, `git mv` this file to `plans/legacy/` per the CLAUDE.md override (keep
the historical record), rather than deleting it.*
