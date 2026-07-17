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
| `--cpu-jobs` / `--profile {wfi,cpu}` (the **partition**, not `--jobs`) | QEMU scenarios oversubscribe host cores → timing flakes | verified dead on the snemu path — see below |
| `--force` + `target/.itest.lock` | concurrent QEMU runs compete for CPU | snemu machines don't share timing |
| `--capture {summary,tail,signal,full}` + `itest-show` | post-mortem for failures you can't reproduce | re-run the one scenario |

Six flag families, one 7-verb subcommand group, and ~1150 lines across
`itest/baseline.rs` + the harness capture plumbing, all resting on one premise.
Remove the premise and they don't need arguing about individually.

### `--jobs` survives — it is not the same `--jobs`

There are **two** `--jobs` in this surface and they must not be conflated:

- **QEMU `itest --jobs` / `--cpu-jobs` / `--profile {wfi,cpu}`** — the wfi/cpu
  *partition*. This is the QEMU artifact.
- **`snemu-itest -j` / `--jobs`** — worker count, defaults to
  `available_parallelism()`. **This survives the promotion and is load-bearing:**
  turning it down is how we measure the impact of scenario packing (the A/B for
  `--order` and the critical-path scheduler), and how we leave host cores free
  for other work. It is a real knob, not a leftover.

Only the *partition* dies. The evidence, checked rather than assumed:

- `CpuProfile`'s own doc says the split exists because *"running two `Cpu`
  scenarios simultaneously can stretch wall-clock past the harness's
  per-scenario timeout"* — a host-wall-clock concern. snemu scenarios are
  bounded by **instret**, not by a wall-clock timeout that host contention can
  blow.
- `snemu_audit.rs` and `schedule.rs` **never read `cpu_profile`**. The
  precedence-aware critical-path scheduler packs by `--order` (wall/instret),
  not by the wfi/cpu split. The classification is already dead on the snemu path.

This is the check Step 0.2 skipped. Unlike `snemu-fork` — where the supposed
replacement covered a *different axis* — here the replacement genuinely doesn't
consult the thing being retired.

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

### Step 0.1 — ✅ DONE: Delete `snemu` (the M1 console-out smoke) and the `minimal-boot` kernel feature

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

### Step 1.0 — ✅ DONE: Stop `itest` from running the host-side checks

**Shipped.** Flag + prerequisite removed; gate documented as
`cargo xtask test && cargo xtask itest` in README, xtask/README, CLAUDE.md, and
[[feedback_commit_gate_repeat_tests]].

**It immediately found two rotted checks** — both hidden for exactly the reason
this step argues, i.e. they live in `cargo xtask test` and the gate everyone runs
is `snemu-itest`:

1. **The loom model-check had been broken since the kernel-core split** — xtask
   still ran `-p kernel-core --test loom_tx` against a package that no longer
   exists. Fixed to `kernel-devices`. `UNIT_TEST_CRATES` *was* updated during the
   split; this was a second hardcoded crate name a few lines below it. Same
   "where the compiler doesn't look" hazard [[project_kernel_core_split_and_wx]]
   already names — one line down from where it was caught.
2. **The `itest-matrix` diagram had drifted by six scenarios** (`hung-detect`,
   `kill-no-cap`, 2× `supervised-shutdown`, `user-on-hart0`, `xhart-kill`) from
   the Hung-detection and Cross-hart-kill commits. Regenerated.

Note the coupling this step *removed* would not have caught either: it only fired
on `itest`, which isn't the gate. The rot was behind the gate, not the flag.



`itest` runs the workspace host checks first and only proceeds if they pass;
`--skip-unit-tests` bypasses. **That flag is the tell** — its only job is to undo
something the command does that you didn't ask for. One verb, one job:
`cargo xtask test` already exists for this and is fast.

Three things make this a clean cut rather than a preference:

- **`snemu-itest` already doesn't do it.** So Step 2.1's promotion would
  *silently drop* the prerequisite. Cutting it deliberately, now, beats having
  the engine swap do it by accident later.
- **The commit gate already skips it.** The gate is `snemu-itest`, which never
  ran the checks. The coupling only fires on the QEMU path — the one being
  demoted — so removing it loses nothing the gate has. It makes an existing hole
  *visible*.
- **Composition is trivial**: `cargo xtask test && cargo xtask itest`.

**Caveat — `run_unit_tests` is misnamed.** It runs the unit tests, *plus* the
loom model-check tests (`kernel-core/tests/loom_tx.rs`, a separate `--cfg loom`
compilation) *plus* the **generated-diagram drift check**. That last one is a
contract gate. Dropping the prerequisite drops all three from `itest`, so the
gate docs must name `cargo xtask test` explicitly — this is the step where
"the gate is `snemu-itest`" stops being sufficient shorthand.

Consider renaming `Cmd::Test` → something that says what it is (`check`?) while
we're here — but that's a separate call, and renaming the thing everyone types is
not free. Flag it, don't bundle it.

**Acceptance criteria**: `cargo xtask itest` runs no host checks; `--skip-unit-tests`
no longer parses; `cargo xtask test` is unchanged; CLAUDE.md's gate section names
`cargo xtask test && cargo xtask itest` rather than implying `itest` covers it.
**RED**: `try_parse_from(["xtask","itest","--skip-unit-tests"])` is an error; a
test asserting the itest path doesn't invoke `run_unit_tests`.
**GREEN**: drop the flag + the prerequisite block in `main.rs`.
**MUTATE**: n/a — removal.
**REFACTOR**: none.
**Done when**: CLAUDE.md's gate is explicit and [[feedback_commit_gate_repeat_tests]]'s
"gate is `snemu-itest`" is updated to the composed form.

### Step 1.1 — ✅ DONE: Add CLI parse tests for the current surface

**Shipped** as `cli_surface_tests` in `xtask/src/main.rs` (6 tests), alongside the
`retired_command_tests` Phase 0 left behind. Covers: all 21 top-level verbs by
name (minimal argv each), an unknown verb rejected (proves it discriminates), the
three subcommand groups requiring a valid member, `itest`'s flake flags (what 2.2
gates), and `snemu-itest`'s perf levers (what 1.3 moves). Plus
`Cli::command().debug_assert()` — clap's own consistency check, which fails at
definition time rather than at someone's terminal.

**The net was proven, not assumed:** renamed `Loc` locally → the suite failed with
`top-level command should parse: ["loc"]`, then reverted. A passing
characterisation test proves nothing until you've watched it fail.

Also fixed two stale `kernel-core` references in help text (`test`, `mutants`) —
same rot family as the loom bug in 1.0; the crate hasn't existed since the split.



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

### Step 1.2 — ✅ DONE: Collapse the snemu family into one subcommand group

**Shipped**: `cargo xtask snemu {boot,diff,fork,profile,bench}`. Five top-level
slots → one. `snemu-itest` deliberately stayed top-level (2.1 renames it to
`itest`; moving it here first would rename it twice).

**Plan correction:** this step was written assuming 0.2 would delete `snemu-fork`,
so it listed the family as four. Fork survived the withdrawal, so it joined the
group — five, not four.

**Two things the work turned up:**

- The family was **not contiguous** in the `Cmd` enum — `SnemuItest` sits in the
  middle of it (boot, diff, fork, *itest*, profile, bench). The lift had to take
  two ranges either side.
- **A test went quietly vacuous.** `retired_command_tests::snemu_m1_smoke_is_gone`
  asserted `["xtask", "snemu"]` is rejected — proving the M1 smoke verb was gone.
  This step took `snemu` as the *group* name, so bare `snemu` now errors as
  "missing subcommand": the assertion still passed, but had stopped testing its
  own name. Deleted rather than left reading as coverage. (The smoke's real
  epitaph isn't a CLI fact anyway — it's that `minimal-boot` is gone from
  `kernel/Cargo.toml`.) *Worth noting the net caught the rename in two places
  first, exactly as 1.1 intended.*

### ~~Step 1.2 (original text)~~

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

### Step 1.3 — ✅ DONE: Move the snemu perf A/B knobs off the itest surface

**Shipped, as `hide = true` rather than a move.** The seven levers (`--jit`,
`--native-jit`, `--tlb`, `--native-ops`, `--no-reg-cache`, `--no-idle-skip`,
`--share-snapshots`) are gone from `snemu-itest --help` but **fully functional** —
every A/B workflow and script keeps working, zero migration. `--speedup` stays
visible, and the command's doc comment names the hidden levers so they stay
discoverable. That meets this step's acceptance criteria exactly ("help no longer
mentions JIT internals" + "levers still reachable") at a fraction of the cost of
rehoming them under the `snemu` group.

**The mutation testing this step called for found the interesting thing.**
`SpeedConfig::resolve` had **one call site and zero tests** — a boolean
precedence lattice, exactly as predicted. Added a matrix test (preset ⊂ tiers,
enable-overrides layer on, `--native-jit` implies the block-JIT frontend, the two
`--no-*` beat the preset). `cargo mutants` over `resolve`/`preset`: **11 mutants,
9 caught, 2 unviable, 0 missed.**

**A live trap, now pinned:** `resolve(None, ..)` falls back to `Low`, so the real
default lives in clap's `default_value = "hi"` — two places. That split has bitten
once already ([[feedback_commit_gate_repeat_tests]]): `--speedup` shipped with no
clap default, silently fell back to `Low`, and the 3× JIT lever sat switched off.
`an_unset_level_falls_back_to_low_not_to_the_cli_default` now pins the fallback so
a future edit that drops the clap default fails loudly instead of getting slow.

### ~~Step 1.3 (original text)~~

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
is a rename or a move across a surface with no other net.

### ~~the quarantine~~ — WITHDRAWN, `itest-harness` is already layered correctly

A draft of this section proposed extracting a `qemu-itest` crate above
`itest-harness`, moving `CpuProfile`, `baseline`, `history`, `lock`, `signature`,
`stats` (Wilson), `aggregate` and `metrics` into it — on the claim that the shared
substrate was full of QEMU-specific flake apparatus. **That was wrong on both
halves of the claim.**

The test that breaks it: *`qemu-itest` should own only what is **only** useful to
QEMU.* Nothing on that list passes.

- **Wilson scores / the z-test / baseline / history / the signature classifier /
  Prom+OTLP** are generic — and not incidentally so. Per
  [open-sourcing-extractables.md](open-sourcing-extractables.md), `itest-harness`
  is the repo's **highest-reuse-value** extraction candidate precisely because it
  *"owns everything that has nothing to do with QEMU or RISC-V"*, and that doc
  names this exact list as the product. The quarantine would have buried the best
  open-sourcing candidate in the repo inside a QEMU-specific crate.
- **`CpuProfile`** fails the test too. It classifies *host-CPU contention* —
  "burns a core" vs "idles on a timer" — which is meaningful for any subject that
  burns host CPU. The open-sourcing plan explicitly wants `RunnerConfig` proven
  against a **non-QEMU subject**; that generality is a feature of the harness, not
  a leak. snemu not reading `cpu_profile` means *unused by one consumer*, not
  *belongs to the other*. Those were conflated.
- **What is genuinely QEMU-only** — process lifecycle, socket cleanup, `-append`
  bootargs — already lives xtask-side in `xtask/src/itest/harness.rs`, exactly
  where the open-sourcing plan says it should. But it is **tangled with
  `LiveSnemu`** there, so even that file isn't purely QEMU. That tangle — not the
  statistics — is the real seam, and it's Phase 3.

**So the layering is already right and Phase 2 should not touch it.** What
survives of Phase 2 is the engine swap (2.1), gating the flake flags to
`--engine qemu` (2.2b), the docs split (2.2c), and the instret baseline (2.3).

### the QEMU runner survives

An earlier draft had `--engine qemu` surviving Step 2.1 while Step 2.2 deleted
`CpuProfile`. **That was incoherent**: the partition is exactly what the QEMU path
needs, and a QEMU runner that fans cpu-bound scenarios out at `--jobs` width blows
its own per-scenario wall-clock timeouts. It would degrade the escape hatch
precisely when you'd reach for it — when you distrust snemu and need QEMU to
arbitrate.

So the QEMU scenario runner **stays, and keeps its machinery**. What changes is
*where that machinery lives*:

> **`qemu-itest`, a new crate layered ABOVE `itest-harness`.**

Today the layering is backwards. `CpuProfile` — a concept whose entire meaning is
"QEMU scenarios contend for host cores" — lives in `itest-harness/src/runner.rs`,
the *shared* substrate. The snemu path links it and ignores it
(`snemu_audit.rs`/`schedule.rs` never read `cpu_profile`). The same is true of
most of the crate: `baseline`, `history`, `lock`, `signature`, `stats` (Wilson
scores), `aggregate`, `metrics` are all flake apparatus sitting in shared space.

The engine-neutral core inside `itest-harness` is small — `Scenario` (name, run,
tags, workload) and `select_by_tags`. So the extraction is close to an
**inversion**: shrink `itest-harness` to the neutral core it was meant to be, and
let `qemu-itest` above it own the rest.

This is the house argument, third time: `kernel::sync`'s chokepoint, the
`kernel-core` split, and this crate's own extraction note — *"we're going to want
this even if no one else ever uses the harness crate — the boundary is the
discipline."* A crate boundary makes "the snemu path grew a dependency on QEMU
flake machinery" a **compile error** instead of something review has to catch.

### what this costs the plan, honestly

Phase 2 is no longer "delete ~1150 lines". Those lines survive, relocated. The
win changes shape:

- the **default surface** gets clean (the flake flags become qemu-engine-only);
- the QEMU apparatus is **contained** — it can sit still without spreading, and
  its cost is visible as a crate rather than diffused through the substrate;
- the snemu path **cannot** regrow a dependency on it.

That's a real win and arguably a better one than deletion — a quarantined escape
hatch keeps QEMU able to arbitrate. But it is not the line-count win the first
draft advertised, and the summary table below is corrected accordingly.

### the open design question: where does the classification live?

`CpuProfile` is a *field on the shared `Scenario`* (`Scenario::cpu_bound`), and
the `catalog!` macro's row grammar (`wfi`/`cpu`) sets it across ~119 rows. If the
concept moves to `qemu-itest`, the shared struct must stop carrying it. Options:

- **(a)** `qemu-itest` owns a `const CPU_BOUND: &[&str]` name list. Simple; risks
  drift from the catalog.
- **(b)** `catalog!` co-generates a third item — a qemu-side classification table
  — alongside `SCENARIOS` and `scenario_view_fn`. The macro already co-generates
  two items *specifically so they can't drift*; this extends the same discipline.
- **(c)** `Scenario` keeps an opaque engine-hints bag. Rejected: that's the
  current leak with extra indirection.

**(b) is the recommendation** — it preserves the anti-drift property the macro
exists for. Settle this before writing 2.2.

### Step 2.1 — ✅ DONE: Make `itest` snemu-backed, QEMU behind `--engine qemu`

**The flip-over is landed.** `cargo xtask itest` runs the snemu audit;
`--engine qemu` runs the QEMU suite unchanged; `snemu-itest` is gone. Both
verified live (snemu: 3/3 + a positional filter; qemu: `boot-reaches-heartbeat`
passed through the old runner). 137/137 xtask tests.

**Two engine-conditional defaults, both nearly merged away silently:**

- **`--jobs`** — snemu wants `available_parallelism()`, qemu wants `10`
  (empirically A/B'd).
- **`--opt`** — snemu wants `Mid`, qemu wants `Low`. This one only existed
  because *concurrent work added `--opt` to the QEMU runner while this step was
  in flight* — the two engines converged on the same concept from opposite sides,
  and the merge would have silently changed which kernel one of them tests.

Both are now `Option<T>` on the merged command, resolved per engine at dispatch,
with the reason written next to the field. **A merged flag whose default differs
per engine is the trap of this whole phase** — the flag looks shared, the default
isn't.

**`--only` is gone**: the positional `scenario` is the filter for both engines.
That leaves one wrinkle documented on the field rather than papered over — qemu
reads it as an *exact name or comma-list*, snemu as a *substring*. An exact name
is safe on both; `itest sched` runs every `sched-*` under snemu and is an
unknown-name error under qemu. Unifying that is 2.2 territory if it bites.

**Process note — the first attempt was reverted.** A script lifted the snemu
fields by walking back from each declaration to the nearest `///`, which silently
kept only the **last line** of every multi-line doc comment. Eleven fields
mangled; caught by eye, not by any test (clap docs aren't type-checked). The
redo moved **one contiguous slice** — no per-field parsing, nothing to truncate —
and asserted every expected flag was present in the lifted text before splicing.
*Structural edits should move blocks, not parse fields.*

### ~~Step 2.1 (original text)~~

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
**REFACTOR**: hold off — Step 2.2 moves much of what's here.
**Done when**: both engines reachable, verdicts match, CLAUDE.md's commands table
updated.

### Step 2.2: Make the flake flags qemu-engine-only

The machinery stays where it is (see above — it's generic and it's the
open-sourcing candidate). What changes is only the **surface**: `--repeat`,
`--fail-fast`, `--force`, `--cpu-jobs`, `--profile`, `--capture` and `itest-show`
are meaningful only under `--engine qemu`, and should parse-error (not silently
no-op) on the snemu path.

This is a clap-level change. No crate moves, no deletions in `itest-harness`.

**`--jobs` is NOT in that list.** snemu-itest's `-j` survives as the promoted
`itest`'s worker knob — it's the packing-measurement lever and the way to leave
host cores free. Quarantining the *partition* must not take the *knob* with it.

**Acceptance criteria**: `itest --repeat 3` errors with a message naming
`--engine qemu`; `itest --engine qemu --repeat 3` works; `itest --jobs 4` works on
**both** engines; `cargo xtask itest <scenario>` reproduces a failure identically
on consecutive runs (demonstrate on a real failing scenario).
**RED**: parse tests for each flag × engine combination; a determinism test
running one scenario twice asserting byte-identical verdict + instret.
**GREEN**: gate the flags on the engine.
**MUTATE**: over the flag/engine compatibility matrix.
**KILL MUTANTS**: as found.
**REFACTOR**: assess whether the two engines want distinct arg structs rather
than one struct with engine-conditional fields.
**Done when**: the determinism demonstration is recorded here.

### Step 2.2c: Rewrite the `.itest-runs/` debugging habit

Separate from 2.2b because it's a docs/memory change, and because the habit
outlives the flag: `.itest-runs/` still gets written **by the QEMU engine**. The
instruction that needs rewriting is the *default* one — on a snemu failure, re-run
the single scenario rather than reading a capture directory that the snemu path
never populated.

**Acceptance criteria**: CLAUDE.md and the `feedback_itest_runs_debugging` memory
distinguish the two paths — snemu: re-run the scenario; QEMU (`--engine qemu`):
read `.itest-runs/<ts>/` as before.
**Done when**: both documents state which engine each workflow belongs to.

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

---

## Phase 3 — untangle the frame sources in `View` — 🟡 PART DONE

**The enum landed** (decision: enum, not trait — the set is closed at three, and
we'd already withdrawn the `qemu-itest` crate the trait was for). `View`'s
`live: Option<LiveSnemu>` + `batch: bool` + `quiet: bool` + `input` + `log_path`
+ `recorder`/`cursor` are now one `source: FrameSource` field:

```rust
struct Streamed { recorder: Arc<Recorder>, cursor: usize }
enum FrameSource {
    Qemu { stream: Streamed, input: Arc<Mutex<Option<ChildStdin>>>, log_path: PathBuf },
    Replay { stream: Streamed },
    Live(LiveSnemu),
}
```

What it bought, concretely:

- **Three dummy fields per constructor are gone.** `View::live` and `View::replay`
  each built an empty `Recorder`, a `Mutex::new(None)` stdin, and an empty
  `PathBuf` log — meaningless values that existed only to satisfy the struct.
- **`batch` and `quiet` were always equal** — both `false` for QEMU, both `true`
  for replay and live. Two booleans that had never disagreed, both secretly
  meaning "not QEMU". Now derived (`is_batch()`/`is_quiet()`), so they *can't*
  disagree. `live: Some` + `batch: false` is no longer representable.
- **Two error messages stopped lying.** `send_input` on a replay view blamed
  "QEMU stdin was not piped" (wrong engine); `wait_for_log` on a replay view
  polled an empty `PathBuf` for the full budget and then blamed the log. Both now
  say what's actually true, and `wait_for_log` fails fast instead of burning the
  budget.

**Verification** (this was a REFACTOR under green, not RED-first — see the caveat
below): `cargo test -p xtask` 116/116, clippy clean, no dead code, and
`snemu-itest` 119/120 — the one failure (`supervised-regrants-caps-on-restart`,
"the intensity guard never tripped Escalate") is unrelated in-flight supervision
work, confirmed by the user, not the refactor.

### the caveat: the enum costs the plan's chosen proof

The RED this phase specified — *a `View` driven by a fake frame source* — is a
**trait** affordance. With an enum, a test double needs a `#[cfg(test)]` variant.
So this landed as a behaviour-preserving refactor guarded by the existing suite
(116 unit + 120 scenarios) rather than proven by a new test. That's a legitimate
REFACTOR, but a weaker proof than "the fake source compiles and drives it".

**Still open:** `View` remains one type with per-variant `match`es rather than
per-backend behaviour, so QEMU is *tidier* but not yet *liftable*. If a
`qemu-itest` crate ever becomes a real goal (it isn't today — the quarantine was
withdrawn), revisit trait-vs-enum then; the enum is a strictly better starting
point than the `Option` was.



**This is the important one**, and it's the enabling refactor everything else
kept bumping into. It is *not* a consolidation step — it's a design fix that the
consolidation work surfaced.

### the tangle

`xtask/src/itest/harness.rs` (1287 lines) mixes **three different frame sources**
behind one `View` type:

| source | constructor | what it is |
|---|---|---|
| QEMU child process | `Boot::view` | `Child` + unix socket + `ChildStdin`; frames arrive on a reader thread |
| live snemu machine | `View::live` | in-process `snemu::machine::Machine`, stepped on demand, instret-budgeted |
| batch replay | `View::replay` | a previously captured frame stream, no guest at all |

`View` selects between them with a **field** — `live: Option<LiveSnemu>` — and
branches on it per operation (`wait_for`, `send_input`, `wait_for_log`). So:

- **QEMU cannot be extracted.** Any `qemu-itest` crate would have to take `View`
  with it, and `View` is what the snemu path uses too. The `Option` field *is* the
  coupling.
- The three sources' differences are real and interesting — the doc comments on
  `LiveSnemu::max_instret` (budget in guest instret, not host `step()` calls, so
  the block JIT can't scan more guest work for the same budget) and on `View::live`
  vs `View::replay` (interactive input→output loops a batch capture can't
  reproduce) are *exactly* the kind of thing a type should encode, not a comment
  next to an `Option`.

### the shape

A frame-source abstraction — trait or enum — carrying the operations `View`
actually needs: advance-until-next-frame, send input, read UART/log, and report a
budget/deadline. Then `View` is a neutral cursor + assertion API over it, and each
source is a backend:

- QEMU backend (`Boot`, `Child`, socket, stdin) — **now extractable** to
  `qemu-itest`, and the only thing that genuinely belongs there.
- live-snemu backend (`LiveSnemu`).
- replay backend.

**Trait vs enum is an open call.** The set is closed and small (three), which
argues enum; but the QEMU variant is what we want to be able to *lift out of the
crate*, which argues trait. Decide when writing it — the deciding question is
whether `qemu-itest` is a real goal or just a tidiness one.

### sequencing

**Phase 3 does not block Phases 0–2**, and Phases 0–2 don't block it. But Phase 3
**is a precondition for any `qemu-itest` crate** — the thing the withdrawn
quarantine was reaching for. If the goal is really "QEMU can't contaminate the
snemu path," this is the step that delivers it, and the quarantine was a
shortcut around it.

Do it before 2.2 if the engine-gated flags turn out to need per-engine arg
structs (2.2's REFACTOR note asks the same question from the other side).

### steps

**Acceptance criteria**: `View`'s public assertion API (`wait_for`,
`assert_absent`, `name_of`, `send_input`, `wait_for_log`) is unchanged from a
scenario's point of view — all ~119 scenarios compile and pass untouched; `View`
holds no engine-specific field; each backend's budget semantics are encoded in its
own type rather than in a comment.
**RED**: a test constructing a `View` over a **fake** frame source — impossible
today, and the proof the abstraction is real. If a hand-rolled test double can
drive `View`, the coupling is gone.
**GREEN**: introduce the abstraction, move the three sources behind it.
**MUTATE**: over the per-source advance/budget logic — `LiveSnemu`'s instret
accounting is the subtle part and mutants should find any off-by-one in it.
**KILL MUTANTS**: as found.
**REFACTOR**: 1287 lines should split along the new seam; check the pieces are
each readable alone.
**Done when**: all scenarios green under both engines, the fake-source test
exists, and `qemu-itest` is *possible* (whether or not we then do it).

---

## Where this lands

**Counting correction:** the opening survey said "22 top-level commands". It was
**24** — miscounted by hand. Verified since with
`xtask --help | grep -E '^  [a-z]'`. Numbers below are measured, not estimated.

| | before | now (Phases 0–1 done) | after Phase 2 |
|---|---|---|---|
| top-level commands | 24 | **20** (incl. `links`, added by other work — so 19 of ours) | ~19 |
| `snemu-itest` visible flags | 14 | **7** (the 7 perf levers hidden) | — |
| `itest` flags (default/snemu path) | 13 | 13 | ~5 (rest `--engine qemu`-only) |
| `baseline` verbs | 7 | 7, but `export`/`push` retargeted at instret (2.3) |
| CLI parse tests | 0 | the whole tree |
| `itest-harness` | already correct | **untouched** |

Plus one kernel feature flag (`minimal-boot`), deleted in Phase 0.

**Both deletion claims are withdrawn.** The first draft promised ~1150 lines
removed; the second promised them relocated. Neither happens: the machinery is
generic, it's the repo's best open-sourcing candidate, and the QEMU runner stays.

**What this plan actually delivers, then:** one dead verb + one dead kernel
feature (Phase 0), a snemu family that reads as a group and an itest surface that
doesn't show you a register allocator (Phase 1), a default engine that's
deterministic with the flake flags gated behind `--engine qemu` (Phase 2), an
instret regression gate (2.3), and CLI parse tests where there were none. That's
a decent, honest outcome. It is not a debloat.

**The pattern worth remembering.** Every deletion claim in this plan that rested
on "X is redundant with Y" was wrong: `snemu-fork` (different axis), the flake
apparatus (generic, not QEMU's), `CpuProfile` (unused ≠ owned elsewhere). The one
that held — `snemu`/`minimal-boot` — was the one with a **single checkable
consumer**. The rule that falls out: *"consumer C doesn't use X" is evidence about
C, not about X.* Before retiring anything here, name the thing that makes it
redundant and check that it covers the same axis — don't reason from who ignores
it.

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

1. `cargo xtask test` — unit tests **+ loom model-checks + the generated-diagram
   drift check** (the `itest-matrix` diagram trips if scenario plumbing moves).
   **Post-1.0 this is no longer implied by `itest` — run it.**
2. `cargo xtask itest` (post-2.1; `snemu-itest` before that) — the deterministic
   gate; a single run replaces `--repeat 10`.
3. `cargo xtask clippy` — **not** `cargo clippy --workspace`.
4. `cargo xtask mutants` where the step has logic (1.3, 2.1, 2.3).

Work lands directly on `main`. Present each step and stop; the user commits.

---
*On completion, `git mv` this file to `plans/legacy/`.*
