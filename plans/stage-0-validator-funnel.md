# Stage 0 — the validator funnel (TDD plan)

**Status:** 📐 **PLAN — not started.** Stage 0 of the bootstrap table in
[../docs/generative-ladder.md](../docs/generative-ladder.md). Pure host work, no
model calls, no GPU: the harness that decides whether a generated candidate is a
training token or garbage, and *reports why* when it is garbage.

Related: [../docs/generative-ladder.md](../docs/generative-ladder.md) (the
bootstrap table, the per-batch report spec, the canon stratum),
[../docs/llm-design.md](../docs/llm-design.md) (corpus tiers),
[babble.md](babble.md) (Tier-0, the null baseline), [drivel.md](drivel.md)
(the rung that consumes what this produces).

**Everything downstream is blocked on this.** The canon in volume (#2) and the
frozen-vocab decision (#3) both need a real corpus, and a real corpus needs a
gate. Tier-0 cannot substitute: babble's lexicon saturates at 571 identifiers,
and that ceiling survived a 33× data increase — it is a property of the
wordlist, not the volume.

---

## The gate

> Harness emits the per-batch report.

Concretely: `cargo xtask cram sift <candidates>` prints the funnel, the coverage
curve, per-recipe yield, and the distribution deltas — for a batch of
hand-written fixtures, before a single model call exists. That is the whole
Stage 0 deliverable.

## What it is not

Not a yield percentage. **The funnel is the product**, because the stage a
candidate dies at is the diagnosis:

| Death stage | Diagnosis | Fix |
|---|---|---|
| parse | generator doesn't know the grammar | exemplar problem — Stage 1 |
| type | knows shape, not semantics | more/better exemplars, tighter recipes |
| run | knows semantics, gets them wrong | recipe must state the postcondition |
| dedup | diversity problem | recipe axis is mined out — new axes |

A single number collapses four different actions into one shrug.

## What already exists (and didn't when the ladder doc was written)

- **The canon gate** (`stitch/tests/canon.rs`) is stages 1–2 in miniature:
  `parse_program` → `lower_items_to_core` → `check_program`, filtered to
  `Severity::Error`. It even ships its own control test proving the type gate
  can fail — gradual typing makes that a live question, and the funnel inherits
  the same hazard.
- **The printer** (`stitch/src/print.rs`, round-trip contract tested in
  `stitch/tests/print.rs`) is what makes the augmentation tier possible at all.
  Alpha-renaming and reordering need an AST→source path; there is now one. That
  is a validator-checked 2–4× multiplier on everything that survives the funnel.
- **`cram-corpus`** already owns corpus assembly, manifest/fingerprint caching,
  and `Layout::{Flat, Printed}`. The funnel's output feeds it; it is the
  consumer, not the host.
- **The oracle** (`stitch/src/oracle.rs`) gives `TokenClass` sets per position —
  relevant to coverage (below) and already the thing babble samples from.

## The name

**`sift`** — the crate, and `cargo xtask cram sift` the verb.

## Decisions

1. **What "run tests" means** — *settled*: the candidate's own native Stitch
   `test` items. See [../docs/stitch-testing-design.md](../docs/stitch-testing-design.md).
   This blocks increment 3: the language feature lands first. The funnel does
   **not** get a private `check()` convention, because the corpus teaches
   whatever convention it contains, and a convention adopted here for expedience
   would become Stitch's de-facto test idiom.
2. **What counts as a grammar production** for the coverage metric — *settled*:
   **AST node-kind + child-shape pairs** (`Match` with 3 arms and a guard is a
   distinct production from `Match` with 1 arm). One AST walk, stable across
   parser refactors, and directly interpretable as "an idiom we have / haven't
   generated". The oracle's `TokenClass` transitions were the alternative: free,
   but they count lexical rather than structural variety.
3. **Where the code lives** — *settled*: a new `sift` crate, depending on
   `stitch` only. `cram-corpus` and `xtask-cram` depend on it. Keeps the funnel
   usable by anything that isn't training (the canon gate, the mutation tester)
   and keeps `cram-corpus`'s job — assembly and caching — a single job.

---

## Increments

Each is RED → GREEN → assess. Every stage is a pure function over `&str` or an
AST; the harness is a fold. No I/O below the CLI increment.

### 1. The funnel report, end to end, with three stages stubbed

The shape first, so every later increment is filling in a hole rather than
renegotiating the interface.

- **RED**: a batch of four hand-written candidates — one parse-fail, one
  type-fail, one run-fail, one clean — produces exact per-stage counts and a
  clean candidate list.
- **GREEN**: `Funnel { generated, parsed, typed, ran, deduped }` +
  `Verdict::{ Died(Stage, String), Survived }` per candidate. Only the parse
  stage is real; the rest pass everything through.

The report is a struct with a `Display`, not a printed string — it has to be
diffable across weeks, so serialization comes free later.

### 2. The type stage

Lift `canon.rs`'s logic into the funnel and **have `canon.rs` call it**, so the
two cannot drift. The canon is the funnel's own regression fixture: every
shipped `.st` file must exit the funnel alive.

- **RED**: canon programs all survive stages 1–2; the `f() -> Int = "not an
  int"` control dies at `Stage::Type` with the checker's message.

### 3. The run stage

**Blocked on native Stitch tests landing**
([../docs/stitch-testing-design.md](../docs/stitch-testing-design.md)) — do
increments 4–8 first if that work is still in flight; nothing else depends on
this one.

- **RED**: a candidate with a failing `test` item dies at `Stage::Run` naming
  the test and the rendered operands; one that faults dies with the fault's
  message and location (diagnostics carry `file:line:col`); one whose tests all
  pass survives; a candidate with **no** `test` items dies at `Stage::Run` too —
  an untested candidate is not a validated token.
- Needs a **fuel cap** — a generated program can loop forever, and the funnel
  must not. The interpreter has fuel and depth guards already; this is choosing
  the budget and reporting exhaustion as its own death cause rather than a
  generic fault.
- **Degenerate tests are the live hazard.** `expect true` passes. The defence is
  mutation testing over the candidate's own AST — we own it, so there is no
  compile-per-mutant. Not in this increment, but the report should carry the
  hook, because a corpus of trivially-tested programs looks identical to a good
  one from the funnel's outside.
- **Constraint from the kernel side: no floats.** Userspace FP is illegal
  (`sstatus.FS` is never set — a float in userspace panics the kernel), and
  canon is validated by use, which means running on the metal. The funnel
  should reject float literals outright at the recipe level rather than let
  them through to a kernel panic later. (Kernel-side FP is being worked in
  parallel; the funnel's rejection is cheap to lift if that lands.)

### 4. Alpha-normalization

The prerequisite for honest dedup: two programs differing only in identifier
choice are the same program.

- **RED**: two candidates differing only in binder names normalize to identical
  token streams; two differing structurally do not; normalization is idempotent.
- Renames binders in binding order (`v0`, `v1`, …); leaves module/native names
  alone (they're part of the meaning). Runs over the AST, prints back through
  `print.rs`.

### 5. MinHash dedup

- **RED**: a near-duplicate (one renamed binder, one reordered independent
  `let`) is caught at the chosen threshold; a genuinely different program with
  the same shape survives; the whole thing is deterministic under a fixed seed.
- Shingles over the alpha-normalized token stream. Threshold is a knob with a
  measured default, not a guess — pick it against the canon plus deliberate
  near-dupes.

### 6. Production coverage + its curve

The diversity metric nobody else gets cheaply, because we own the parser.

- **RED**: a program exercising `match` with three arm shapes reports three
  distinct productions; the empty program reports none; coverage of the canon is
  a specific, asserted number (a ratchet, like `the_canon_is_not_empty`).
- The **curve vs. candidates generated** is the actual output — a plateau means
  that recipe axis is mined out. Emit it as a series, not a scalar.

### 7. Per-recipe yield and per-recipe dedup rate

Great yield + dup factory is a real failure mode and invisible in the aggregate.

- **RED**: a batch tagged with two recipes reports them separately; a recipe
  that produces only duplicates shows 100% yield and ~0% dedup survival.

### 8. Distribution-vs-real deltas

Early warning for "legal Stitch, but nothing like Stitch-as-written".

- **RED**: shape statistics (nesting depth, match arity, function length) and
  identifier entropy computed over a fixture batch match hand-computed values;
  the delta against the canon is zero when the batch *is* the canon.

### 9. The recipe-tuple generator

domain × constructs × size × shape × 3 random must-use words.

- **RED**: the same seed yields the same recipe sequence; the axes cross
  exhaustively before repeating; must-use words are drawn from the real
  corpus's identifier distribution, not babble's 571.
- Recipes are data, and the report keys on them — which is why increment 7
  comes first.

### 10. The CLI + the machine-readable report

`cargo xtask cram sift` — reads candidates, writes the report as human text and
as a machine-readable file beside it, comparable across weeks.

- Follows the repo's CLI conventions (stream separation, `--json`); see the
  `cli-design` skill.

### 11. The augmentation tier

Only once the funnel is trusted, because it multiplies whatever it is given.

- **RED**: an augmented program is validator-clean, alpha-distinct from its
  source, and semantically identical (same `check()` result).
- Alpha-renaming + independent-statement reordering, each augmentation re-run
  through the full funnel. 2–4× on survivors.

### Then: the overnight local run

27–32B, already installed, costing electricity. Stage 2's gate is yield ≥ ~40%
with coverage still climbing — which this harness is what measures.

---

## Not doing next

**More Tier-0 volume.** It is the cheapest lever and it is spent — the phrase
tail survived a 33× data increase, so ~570 identifiers is the ceiling.
