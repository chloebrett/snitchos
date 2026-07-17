# Consolidating the xtask surface

`cargo xtask` has 22 top-level commands and ~14k lines. That is not itself a
problem — xtask is where every orchestration concern in the workspace lands, and
a workspace this wide will have a wide driver. The problem is that a
recognisable fraction of the surface is **scaffolding that outlived its
scaffold**: verbs that proved a thing which has since shipped, and an entire
statistical subsystem that exists to cope with a nondeterminism we are about to
stop having.

The organising decision is this:

> **snemu becomes the test runner; QEMU becomes the fidelity oracle.**

`snemu-itest` is deterministic and much faster than the QEMU suite, and the
commit gate is already `cargo xtask snemu-itest`
([feedback: commit gate](../.claude/CLAUDE.md) records the change in practice —
this plan makes the CLI admit it). QEMU keeps a real job, just not the inner-loop
one: `snemu-diff` is what proves snemu still tells the truth. That reframing is
what makes the rest of the consolidation follow rather than being a taste
argument about command counts.

## what determinism actually deletes

The prize is not "fewer verbs". It's that **most of `itest`'s flag surface is
flake infrastructure**, and flake infrastructure is meaningless against a
deterministic engine. `itest` carries 13 flags; `snemu-itest` carries 13 more.
Sorting them by *why they exist* is the whole plan:

| Flag / feature | Exists because | Under determinism |
|---|---|---|
| `--repeat N` | flake rate must be *sampled* | a scenario fails 0% or 100%; sampling learns nothing |
| `--fail-fast N` | confirm flakiness cheaply | nothing to confirm |
| `baseline` (7 verbs, Wilson scores, `history`) | estimate per-scenario flake rate from noisy runs | there is no rate to estimate |
| `--jobs` / `--cpu-jobs` / `--profile {wfi,cpu}` | QEMU scenarios oversubscribe host cores → timing flakes | snemu-itest already has `-j` + a precedence-aware scheduler |
| `--force` + `target/.itest.lock` | concurrent QEMU runs compete for CPU | snemu machines don't share timing |
| `--capture {summary,tail,signal,full}` + `itest-show` | post-mortem for failures you can't reproduce | re-run the one scenario |

Six flag families, one 7-verb subcommand group, and ~1150 lines across
`itest/baseline.rs` + the harness capture plumbing, all resting on one premise.
Remove the premise and they don't need arguing about individually.

**This is the controversial part, and it is deliberately last.** Phases 0–2 are
free wins that stand on their own even if we never promote `itest`.

### the thing to preserve deliberately

`.itest-runs/` post-mortem debugging is a documented habit, not an accident: *"on
failure, read `.itest-runs/<ts>/` (capture.json frames + signature, .log UART)
FIRST; don't re-run with manual UART capture."* Determinism replaces it with
something strictly better — re-run the single scenario — but the replacement must
land **in the same change** that stops writing the directory, including the
CLAUDE.md line. A habit pointed at a directory that silently stopped existing is
worse than either state.

Corollary worth stating plainly: if we ever *need* the flake statistics back,
that is not a reason to have kept them. It's a signal snemu lost fidelity, and
`snemu-diff` is the instrument that should say so.

## a note on TDD here

xtask has 92 tests, but **none of them parse the CLI** — `main.rs`'s only test
module covers `should_scrub_env_key`. Every step below that reshapes the command
tree therefore gets a genuine RED: `Cli::try_parse_from([...])` assertions over
the argv we claim to accept and reject. That is not ceremony. It's the first time
the surface will have a test at all, and it's what makes the later renames safe
to do mechanically.

Deletion steps (0.1, 0.2) are the honest exception: the RED for "this verb is
gone" is `try_parse_from` *rejecting* it, which is a real assertion, but there is
no behaviour to mutation-test. Don't manufacture one.

---

## Phase 0 — deletions (free, uncontroversial)

Two verbs whose stated purpose is already served by shipped work. Neither has a
consumer outside its own help text.

### Step 0.1: Delete `snemu` (the M1 console-out smoke) and the `minimal-boot` kernel feature

Its own doc comment says it: *"throwaway M1 console-out smoke; superseded by
`itest`/`boot --snemu` once snemu models Sv39 + virtio in M2"*. M2 shipped long
ago. The feature `minimal-boot` exists **solely** to serve this verb — so this
deletes a kernel `#[cfg]` axis (`kernel/src/main.rs`, 4 sites), not just an xtask
arm.

**Acceptance criteria**: `cargo xtask snemu` is rejected by clap with an unknown-
subcommand error; `minimal-boot` appears nowhere in `kernel/Cargo.toml` or
`kernel/src/`; `cargo xtask snemu-itest` stays green at its current count.
**RED**: `Cli::try_parse_from(["xtask", "snemu"])` is an error.
**GREEN**: drop `Cmd::Snemu`, `fn snemu()`, the feature, and its four `#[cfg]`s.
**MUTATE / KILL**: n/a — pure deletion, no new behaviour.
**REFACTOR**: check whether `qemu::build_kernel` still needs its feature-slice arg
for any other caller.
**Done when**: snemu-itest green, README's `snemu` mention gone.

### ~~Step 0.2: Delete `snemu-fork`~~ — WITHDRAWN, the justification was wrong

The original claim was that `snemu-fork`'s purpose (proving boot amortization)
shipped as `snemu-itest --share-snapshots`, so it was a demo of a capability we
already ship. **That is false, and the deletion was reverted.**

The two share a *design*, not a *capability*:

- `--share-snapshots` collapses scenarios **of the same workload** — see
  `snapshot_tree.rs`: *"Two scenarios sharing a workload coincide up to their
  first injection."* It never patches the DTB.
- `snemu-fork` forks one boot across **different workloads**, by overwriting the
  `workload=` bootarg in the booted snapshot's RAM. It works only because
  `workload_dtb` pads the bootarg to a fixed 40-char field, so every workload's
  DTB is byte-identical in size and the overwrite is layout-preserving.

That fixed-width trick is subtle, hard-won, and lives nowhere else. Deleting
`snemu-fork` would delete the only working demonstration of cross-workload boot
amortization — a real unrealized optimization for the suite, which today still
boots once per workload group.

**How the error happened, so it doesn't recur:** `plans/snemu-multi-hart.md` says
*"Reuses the snapshot design `snemu-fork` already proved."* That sentence says the
design was reused; it was read as saying the capability was replaced. A citation
is not an equivalence. This is exactly the case the
[don't-retire-distinct-path-coverage](../.claude/CLAUDE.md) rule exists for, and
the rule caught it — one layer later than it should have.

**If anything, the follow-up points the other way:** applying the DTB-patch trick
to `snemu-itest` would let one boot serve all ~20 workloads instead of one per
group. That's a candidate optimization, not a deletion. Out of scope here.

---

## Phase 1 — regrouping (mechanical, low risk)

### Step 1.1: Add CLI parse tests for the current surface

The safety net Phase 2 needs, written **before** anything moves. Characterisation,
not design: assert what the tree accepts *today*.

**Acceptance criteria**: every top-level verb has a `try_parse_from` test pinning
its name and its non-default flags; the suite fails if a verb is renamed or a flag
dropped.
**RED**: the tests don't exist; write them, watch them pass, then rename one verb
locally and confirm the test catches it (prove the net has no hole).
**GREEN**: n/a — characterisation of existing behaviour.
**MUTATE**: `cargo xtask mutants` over the parse helpers if any logic lands.
**REFACTOR**: none expected.
**Done when**: `cargo test -p xtask` covers the tree; the deliberate-rename check
was performed and reverted.

### Step 1.2: Collapse the snemu family into one subcommand group

After Phase 0 the family is `snemu-boot`, `snemu-diff`, `snemu-bench`,
`snemu-profile` — four top-level slots that all mean "do a thing under snemu".
`cargo xtask snemu {boot,diff,bench,profile}` reads better and takes one.

**`snemu-itest` deliberately stays at top level** and is *not* pulled into the
group: Phase 3 renames it to `itest`. Moving it here first would rename it twice.

**Acceptance criteria**: `cargo xtask snemu boot --workload smp` works;
`cargo xtask snemu-boot` is rejected; each subcommand's flags are unchanged.
**RED**: `try_parse_from(["xtask", "snemu", "boot", "--workload", "smp"])` parses
to the expected variant.
**GREEN**: a `SnemuCmd` subcommand enum; dispatch arms move under it.
**MUTATE / KILL**: n/a — no logic change.
**REFACTOR**: the four impls stay in their modules; only the enum moves.
**Done when**: README + `xtask/README.md` updated.

### Step 1.3: Move the snemu perf A/B knobs off the itest surface

`snemu-itest` carries seven perf levers — `--jit`, `--native-jit`, `--tlb`,
`--native-ops`, `--no-reg-cache`, `--no-idle-skip`, `--share-snapshots` — plus
`--speedup` and `--order`. These are **snemu development** knobs: their whole
point is the oracle A/B ("on vs off must be byte-identical, only faster"). Once
this command is the everyday `itest`, someone running the test suite should not
be looking at a register allocator.

Keep `--speedup {low,med,hi,extra}` on the itest surface — one dial, four
positions, already the documented way to pick a regime. Route the individual
levers through the `snemu` group (or an env var read by `SpeedConfig::resolve`)
where the A/B work lives.

**Acceptance criteria**: `snemu-itest --speedup low` still selects the idle-skip-
only baseline; the individual levers still reachable for A/B work; the itest
help text no longer mentions JIT internals; `SpeedConfig::resolve`'s behaviour is
unchanged for every input it accepts today.
**RED**: a test pinning `SpeedConfig::resolve` across the preset × override
matrix — this is real logic and the one place in Phase 1 that earns mutation
testing.
**GREEN**: narrow the clap surface; keep `resolve` intact behind it.
**MUTATE**: `cargo xtask mutants` over `resolve` — the preset/override precedence
is exactly the kind of boolean lattice mutants find holes in.
**KILL MUTANTS**: strengthen the matrix test until survivors are equivalent or
gone.
**REFACTOR**: assess whether `SpeedLevel` + the overrides want one config struct.
**Done when**: `docs/snemu-perf-options.md` reflects the new access path.

---

## Phase 2 — the promotion (substantial; the controversial one)

**Do not start Phase 2 until Phase 1's parse tests are green.** Every step here
is a rename or a deletion across a surface with no other net.

Sequenced so the engine swap and the flake-machinery removal are **separate
commits**. If the promotion needs to be reverted, we want to revert the swap
without resurrecting 1150 lines by hand.

### Step 2.1: Make `itest` snemu-backed, QEMU behind `--engine qemu`

The rename, and only the rename. The flake machinery stays alive (and reachable
via `--engine qemu`) through this step, precisely so this step is revertable.

**Acceptance criteria**: `cargo xtask itest` runs the snemu audit and reports the
current expected count; `cargo xtask itest --engine qemu` runs the old QEMU suite
unchanged; `cargo xtask snemu-itest` is rejected; a scenario name positional works
against both engines.
**RED**: parse tests for both engine paths + a run of each proving the verdicts
match the pre-rename baselines.
**GREEN**: rename the variant, add `--engine`, dispatch to the two runners.
**MUTATE**: over the engine-dispatch + flag-compat logic.
**KILL MUTANTS**: as found.
**REFACTOR**: hold off — Step 2.2 deletes half of what's here.
**Done when**: both engines reachable, verdicts match, CLAUDE.md's commands table
updated.

### Step 2.2: Retire the flake machinery with the QEMU runner

The payload step. Delete `--repeat`, `--fail-fast`, `--force` + `.itest.lock`,
`--jobs`/`--cpu-jobs`/`--profile`, `--capture` + `itest-show`, and the
`.itest-runs/` history writer.

**Ships with the CLAUDE.md rewrite of the `.itest-runs/` debugging habit — not
after it.** The replacement instruction is "re-run the single scenario; it will
fail the same way."

**Acceptance criteria**: `cargo xtask itest <scenario>` reproduces a failure
identically on consecutive runs (demonstrated on a real failing scenario — the
~2 standing snemu FS-read fidelity gaps are the natural subject); none of the
retired flags parse; `.itest-runs/` is no longer written; CLAUDE.md and the
memory note describe the new workflow.
**RED**: parse tests reject each retired flag; a determinism test runs one
scenario twice and asserts byte-identical verdicts + instret.
**GREEN**: delete the flags, the lock, `itest/baseline.rs`'s flake paths, the
capture levels, `itest-show`.
**MUTATE**: over whatever survives in the harness.
**KILL MUTANTS**: as found.
**REFACTOR**: `itest/harness.rs` (1287 lines) should shrink hard here — reassess
what it's still for.
**Done when**: the determinism demonstration is recorded in the plan, CLAUDE.md
updated, `cargo xtask itest` green.

### Step 2.3: Reframe the baseline as an instret gate

The one part of `baseline` with a life after determinism — but its **subject**
must change. Tracking pass rate over time is pointless when it's always 1.0.
Tracking **per-scenario instret** is not: it's a deterministic number that
regresses when the kernel gets slower, and `snemu-itest` already collects it.
`export` (Prometheus textfile) and `push` (OTLP) keep their plumbing; the flake
verbs (`promote`/`discard`/`recover`/`adopt`, Wilson scores, `history`) go.

This is where two existing threads meet: the
[per-itest instret breakdown idea](../.claude/CLAUDE.md) wants exactly this
number, classified by behaviour. This step delivers the total; the breakdown is
its natural sequel and is **out of scope here**.

**Acceptance criteria**: `cargo xtask baseline export <path>` emits per-scenario
instret in Prometheus textfile format; a deliberate kernel slowdown moves the
number and a re-run of the same tree reproduces it exactly; the flake verbs are
gone.
**RED**: a test asserting the exported metric names/values for a fixed audit
result; a reproducibility test (same tree → same instret).
**GREEN**: retarget the exporter at instret; delete the flake verbs.
**MUTATE**: over the exporter's formatting + the threshold comparison.
**KILL MUTANTS**: as found.
**REFACTOR**: `baseline.rs` (391 lines) should shrink to the export/push core.
**Done when**: the gate demonstrably catches a synthetic regression.

---

## Where this lands

| | before | after |
|---|---|---|
| top-level commands | 22 | ~15 |
| `itest` flags | 13 | ~5 |
| `baseline` verbs | 7 | 2 (`export`, `push`) |
| CLI parse tests | 0 | the whole tree |

Plus a kernel feature flag (`minimal-boot`) and roughly 1150 lines of flake
apparatus.

## What this plan explicitly does not do

- **Touch `boot` / `debug` / `collect` / `reader` / `stack` / `measure` /
  `diagram` / `loc` / `audit` / `snip`.** They're 10 of the 22 and none of them
  is scaffolding — each names a distinct job. A wide workspace gets a wide
  driver; that's not the problem being solved.
- **Remove QEMU.** Its job narrows to the oracle (`snemu diff`), which is a job
  that gets *more* load-bearing, not less, once the suite depends on snemu
  telling the truth.
- **Build the per-scenario instret breakdown.** Step 2.3 lands the number; the
  behavioural classification is its own piece of work.

## Pre-commit gate (per step)

1. `cargo xtask itest` (post-2.1; `snemu-itest` before that) — the deterministic
   gate; a single run replaces `--repeat 10`.
2. `cargo xtask clippy` — **not** `cargo clippy --workspace`.
3. `cargo xtask mutants` where the step has logic (1.3, 2.1, 2.2, 2.3).
4. `cargo xtask test` — includes the generated-diagram drift check, which the
   `itest-matrix` diagram will trip if scenario plumbing moves.

Work lands directly on `main`. Present each step and stop; the user commits.

---
*On completion, `git mv` this file to `plans/legacy/`.*
