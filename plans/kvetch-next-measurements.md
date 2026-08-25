# Plan: kvetch — what the nats actually buy

**Branch**: none — work lands directly on `main` (project rule)
**Status**: Active

## Goal

Find out whether the ladder's measured loss improvements mean anything for the task
the model exists to do, and which of the remaining levers is worth spending days on.

## Context

[batch11 findings](../notes/batch11-training-findings.md) settled three things across 23
runs: corpus volume has largely stopped paying (+55.7% bought 0.025 nats), the 30
hand-polished exemplars are worth ~13× their token count, and **scaling the rung
overturned "corpus first"** — quip beats drivel by 0.258 nats at convergence.

It also opened one uncomfortable gap. Across the full range the project has produced —
**2.688 → 2.257, a 0.43-nat / 35% perplexity improvement** — four sample comparisons
found **no trend in parse rate and no perceivable difference in output**. Two readings:

- **Pre-threshold.** Every rung is still "correct shape, no meaning"; coherence appears
  further up. Loss tracks invisible progress. → climb the ladder.
- **Proxy drift.** Held-out NLL is dominated by the ~46% of tokens that are English
  comments, which a 3M model cannot model and which do not matter for Tab completion.
  → the metric and the product have parted company.

**Steps 1 and 2 exist to tell these apart before steps 3–5 spend days of compute.**

### Measurement discipline (non-negotiable — each was learned the hard way)

- Frozen `corpora/kvetch-batch9.vocab` and frozen held-out on **every** run.
- Always `--name`; without it a sweep overwrites itself.
- `--eval-batch 1024`, never the default 64.
- **Two seeds per arm.** Comparisons are paired — arms at the same seed share weight
  init *and* held-out eval windows, which is what resolved effects at 0.0055.
- Do not filter the corpus. Dropping parse deaths cost 0.37 nats; stripping comments
  cost 0.27.
- **Absolute NLL is not comparable across different held-out sets or vocabs.** Only
  deltas within a condition are.
- Do not promote a checkpoint. `drivel-b9b10-30k` is committed, itest-asserted
  byte-exact, and embedded; a quip promotion is a **kernel-image budget** decision
  first (12.2 MB against 4.2 MB, on an image that already hit `OutOfFrames` at 4.5 MB).

## Acceptance Criteria

- [ ] A checkpoint can be scored against arbitrary held-out text, and the scorer is
      **verified against known ground truth** rather than trusted.
- [ ] We know whether quip's 0.258-nat advantage over drivel survives on code-only
      text, or whether it was bought in English prose.
- [ ] A completion-shaped metric exists that measures the actual task (continue a
      real prefix), not free generation.
- [ ] We know whether the volume knee was corpus *saturation* or corpus *redundancy*.
- [ ] Each exploration ends with a written finding — including the null results.

## Steps

Every code step follows RED-GREEN-MUTATE-KILL MUTANTS-REFACTOR. No production code
without a failing test. Experiment steps have a protocol instead, and a written finding
as their deliverable.

---

### Step 1: Expose a scoring entry point in `cram`

**Acceptance criteria**: `cram` offers a public function that, given a decoded model, a
held-out token stream and a `TrainingConfig`, returns the same mean held-out loss the
training loop computes. No second implementation of the forward pass.
**RED**: A test that scores a model over a known token stream and asserts the value
equals what `train`'s held-out pass produces for the same weights, config and seed.
**GREEN**: A `pub fn` wrapping `TrainingConfig::held_out_batch` (already public,
`run.rs:109`) and `mean_loss_and_gradient` (private, `run.rs:311`). Reuse the gradient
path — `run.rs` deliberately avoids a forward-only loss because two forward passes that
disagree is a worse bug than the wasted milliseconds.
**MUTATE / KILL MUTANTS / REFACTOR**: per skill.
**Done when**: green, and the loss is provably the trainer's own number.

### Step 2: `--score` mode in `xtask-cram`, verified against ground truth

**Acceptance criteria**: `cargo xtask cram --score <checkpoint> --eval-vocab <vocab>
--held-out-root <dir> [--strip-comments] --eval-batch 1024 --seed <n>` prints a
held-out NLL. **Scoring `quip-D-60k-s1` against unstripped `corpora/heldout` at seed 1
reproduces 2.2571**, the exact figure its training run's final row reported.
**RED**: An integration test asserting that reproduction against the committed
ground-truth value.
**GREEN**: Arg plumbing + decode checkpoint/vocab + load corpus + tokenize + score.
Reuse `strip_comments` (`xtask-cram/src/corpus.rs:204`, already thoroughly tested,
including `"//"` inside a string literal).
**MUTATE / KILL MUTANTS / REFACTOR**: per skill.
**Done when**: the ground-truth test passes. *A wrong instrument does not error — it
quietly agrees with itself. This test is the whole point of the step.*

### Step 3 (experiment): did the nats go into English or into code?

**Protocol**: score `drivel-D-b9b10b11{,-s1}` and `quip-D-60k{,-s1}` against
`corpora/heldout`, both unstripped and `--strip-comments`. Eight scores, seconds of
compute.
**Read it as**: only the **quip-minus-drivel gap within each condition** is meaningful —
absolute NLL is not comparable across stripped and unstripped. Gap holds near 0.26 on
code-only text → the improvement is real and we are pre-threshold. Gap collapses → a
large share of 0.43 nats bought English prose.
**Caveat to record**: stripped text is off-distribution for both models, equally. This
measures whether the advantage survives, not "how good are they at code".
**Deliverable**: a finding appended to the batch11 note.
**If ambiguous**: consider the cleaner variant — per-token losses on unstripped text,
partitioned by comment membership. Same text, same windows, no distribution shift, but
it needs a token-aligned comment mask the tokenizer does not currently produce.

### Step 4: a completion-shaped metric

**Acceptance criteria**: given a held-out program, cut at a boundary, the harness asks a
checkpoint to continue and scores the continuation. Reported over the whole held-out
set, not three samples.
**RED**: A test over a hand-built fixture — a known prefix and a known continuation —
asserting the scorer's verdict.
**GREEN**: Minimum implementation. Scoring ladder, cheapest first: (a) exact-token
agreement on the next *k* tokens; (b) does the completed program parse; (c) does it
typecheck in context. Start at (a); it needs no interpreter and is a real signal.
**Design note**: this is the instrument the project is missing. Free-generation parse
rate is not Tab completion, and it has now failed four times to resolve checkpoints half
a nat apart. Prefer the lowest level that discriminates.
**Done when**: the metric separates `drivel-all-30k` (2.688) from `quip-D-60k-s1`
(2.257) — the widest gap available. **If it cannot separate those two, the metric is
not sensitive enough and needs rework before it is used to judge anything.**

### Step 5 (experiment): saturation or redundancy? — foreign-code mix-in

**Protocol**: mix-in first, because it needs **zero new code** — the batch loader takes
any `*.st` in a manifest-less directory, so a directory of foreign source renamed `.st`
trains today. Use the repo's own ~4.8 MB of Rust (comparable to batch11's 5.6 MB) to
avoid a download; there is no network in the agent sandbox.
**Arms**: quip@D versus quip@D + Rust, two seeds, 60k steps, everything else frozen.
**Measured input**: Rust tokenizes at **2.41 bytes/token** under the frozen Stitch
vocab against Stitch's 3.23 — a 34% efficiency penalty, and the honest cost of not
retraining the vocab.
**Read it as**: batch11 bought little, but it was *the same 500-domain recipe sheet*.
Foreign code is maximally non-redundant, so this separates "the corpus is saturated"
from "those particular tokens were redundant" — the hypothesis batch11 raised and could
not resolve.
**If it shows signal**: a two-phase pretrain→finetune is the version likely to work
(standard practice ends on the target distribution), and needs an `--init-from
<checkpoint>` flag — `train()` currently always starts from `pseudo_random_weights`.
That is a separate step, only worth building if mix-in is not harmful.

### Step 6 (experiment): LR sweep for quip

**Protocol**: quip@D at 60k, LR ∈ {1e-3, 2e-3, 3e-3}, two seeds each. 3e-3 is inherited
from drivel; `cram/src/run.rs:55` does not scale LR by rung.
**Why**: quip's seed spread stayed **0.0409 after converging**, still 3–10× drivel's
0.004–0.016. The batch11 note predicted convergence would shrink it and it only fell
29%, so the un-converged-endpoint explanation is refuted. A mistuned LR letting seeds
settle into different optima is the standing suspect.
**Deliverable**: a best LR, and either an explanation of the seed spread or its
elimination. Also revisits whether quip's 0.258 is understated.
**Cost note**: quip parallelises badly — two runs give ~11 300 tok/s each against 20 500
solo (1.1× aggregate, where drivel got 1.78×), because quip's matmuls already saturate
the AMX unit. **Plan quip work as effectively serial**: ~1h40 per run.

### Step 7 (experiment): cliché at 10M

**Protocol**: cliché on the D corpus, two seeds, step count chosen so it converges (quip
needed 60k; expect more). Days of compute — **do not start before steps 3 and 4 land**,
or there will be no way to tell whether it worked.
**Why it is no longer ruled out**: batch9 dismissed cliché because Chinchilla wants
~200M tokens and we have 6.7M. But quip sits at 2.2 tokens/param — ~9× "starved" by that
same argument — and won by 0.258 anyway. **The reasoning that ruled out cliché is the
reasoning that wrongly ruled out quip.** Test it rather than infer it.
**Deliverable**: either the next rung, or the first measured ceiling on the ladder.

## Pre-PR Quality Gate

1. Mutation testing on the code steps (1, 2, 4) — run `mutation-testing` skill
2. Refactoring assessment — run `refactoring` skill
3. `cargo nextest run -p cram -p cram-eval -p xtask-cram` green; `cargo clippy -p <crate>`
4. `cargo xtask links` — this plan links to notes and posts
5. Every experiment step ends with a written finding, **including nulls** — the four
   no-difference sample comparisons are a result, not an absence of one

---
*On completion, `git mv` this file to `plans/legacy/` — do not delete it (project rule).*
