# drivel — the 1M model (TDD plan)

**Status:** 📐 **PLAN — not started.** Rung 1 of the
[generative ladder](../docs/generative-ladder.md): the first rung with weights.
This plan is a **tracer bullet for the training infrastructure**, not an attempt
to build a useful model. The deliverable is a working pipe — corpus → frozen
vocab → training rig → checkpoint → evaluation — proved by drivel *marginally*
outscoring [babble](babble.md) on one honest metric. A model that is barely
better than no model is a complete success here.

Related: [../docs/llm-design.md](../docs/llm-design.md) (runner, corpus tiers,
the four oracle consumers), [../docs/generative-ladder.md](../docs/generative-ladder.md)
(the rung ladder, vocab freeze law, checkpoint manifest, bootstrap gates),
[../docs/babble-design.md](../docs/babble-design.md) (the rung-0 baseline),
[../docs/randomness-and-entropy.md](../docs/randomness-and-entropy.md) (seed
discipline — applies to training-data shuffling and sampling alike).

**Independent of babble increments 11–13** (seed derivation, `user/kvetch`, the
itest). Those are the *serving* hat and are landing separately; nothing here
waits on them, and nothing here touches kernel-adjacent surface.

---

## What "outperform babble" means (decided before anything is built)

babble is **100% parse-valid by construction**. That kills the obvious metric
and constrains the honest ones:

- **`unconstrained-parse%` is NOT a babble comparison.** babble scores 100%
  trivially; drivel can only tie or lose. Its actual roles: a *grammar-learnability
  probe* (increment 5), and the axis on which drivel→quip→cliché are compared to
  each other later. Recording it against babble's 100% is a category error and the
  eval report should label it as such.
- **The headline metric is held-out masked NLL.** At each position of a held-out
  real-Stitch program, the oracle gives the legal class set; both babble (via its
  bias tables) and drivel (via its logits, renormalized over the legal set) are
  *distributions over that set*. Score each by mean negative log-likelihood of the
  token a human actually wrote. Exact, apples-to-apples, no sampling, and stable on
  a few thousand held-out tokens — which is all we will have.
- **Generation metrics are deferred, not dropped.** FIM exact/edit-distance match,
  idiom-match vs the gold set, and shape-distance to real Stitch are the right
  metrics *at corpus scale*. At 2K lines they are too noisy to call a marginal win.
  The harness computes them from increment 3 onward and the report prints them;
  they are simply not the gate.

**The win condition for this whole plan:** drivel's held-out masked NLL is lower
than babble's, on real Stitch neither model was trained on. Nothing else.

### Why 2K lines is enough (and where it stops being enough)

Total real Stitch today is ~2K lines ≈ 20K tokens. Against a 1M-param model that
is ~50× over-parameterized, so **memorization is the expected behaviour, not a
bug to be surprised by**. It does not invalidate the tracer, because masked NLL
is measured on a held-out split the model never saw, and because the *learnable
signal at this scale is exactly the shallow stuff*: which identifiers recur, that
`let` is followed by a name then `=`, typical line and arity lengths. Uniform-
over-legal has none of that, so a margin is genuinely expected.

Three things stretch the data without lying about it, in increasing order of
suspicion:

1. **The held-out split is by program, not by line** — a held-out program shares
   no lines with training, and alpha-normalized MinHash dedup enforces it.
2. **Semantics-preserving augmentation** (alpha-renaming, reordering, extract/
   inline) is a validator-checked 2–4× multiplier and is exactly what
   [llm-design](../docs/llm-design.md) Tier-0 prescribes. Augmented variants of a
   *training* program must never leak into held-out.
3. **Heavy repetition** of the real corpus in the mix (the ladder doc's canon
   up-weighting). ≤4 epochs is the published knee; beyond that, expect nothing.

**Let the eval pick the size.** A run is minutes at this scale, so sweep
{0.25, 0.5, 1}M rather than pre-committing to 1M. If 0.25M wins on held-out NLL,
that is the finding, and it is the bottom point of the ladder's scaling curve —
which the ladder doc explicitly wants measured rather than assumed. "drivel" names
the rung, not a parameter count we are married to.

## Order of execution (not the increment numbering)

Increments are units of work; this is the order they land in. **The
grammar-learnability probe comes first**, because it has no corpus dependency at
all: babble generates unlimited valid training data, held-out is more of the
same, and `unconstrained-parse%` needs only the `stitch` parser. That makes it
the shortest path to an end-to-end pipe — vocab → corpus → train → export → load
→ generate → measure — with data scarcity, train/held-out leakage, and corpus
curation all held out of the picture.

> **1 (trainer) → 4 (rig) → 5 (probe) → 2 (corpus) → 3 (harness) → 6 (tracer) → 7 (manifest)**

The real-corpus work rides on infrastructure the probe has already proved. If the
probe fails, increments 2/3/6 were never going to work and the corpus effort is
saved — which is the point of running it first.

### The probe vocab is NOT the frozen vocab

The trap this ordering creates, stated before it bites: a vocab trained on babble
output is trained on *uniform-over-legal* text — wordlist identifiers, no idiom,
no real token frequencies. It is exactly wrong for real Stitch, and freezing it
would bind the entire ladder to an artifact derived from the null model.

So: increment 5 uses a **disposable probe vocab**, regenerable from a seed and
pinned by nothing. The freeze — increment 1's hash test — happens later, once
increment 2's real corpus exists, and the frozen vocab is trained on the
*training split only*. The freeze is not "the first vocab we trained"; it is "the
vocab the ladder ships with". Probes freeze nothing.

## Placement decisions

Two load-bearing calls, and neither is about drivel:

**(a) The forward pass and the tokenizer are shared `no_std` code; only the
training loop is host-heavy.** Same principle that put the oracle in `stitch`
rather than duplicating grammar knowledge — one implementation, so host training
and on-target serving cannot drift.

**(b) A rung is a config plus a checkpoint, never a crate.** drivel, quip,
cliché, ballad and saga differ *only* in hyperparameters over one frozen vocab
and one architecture. This is precisely the
[runtime-workload](../docs/runtime-workload-selection-design.md) pattern already
in the kernel: one registry, selected by name, purely additive — adding a rung is
a `Rung` variant and a checkpoint, not a build variant and not a new crate. There
is no `drivel-model`; there is `kvetch-model` with `Rung::Drivel`.

### Naming: **kvetch** infers, **cram** trains

`kvetch` is the model subsystem and the on-target inference engine (already
reserved in [llm-design](../docs/llm-design.md)). **`cram`** is the host-side
pipeline that produces what kvetch serves — corpus, vocab, training, export.

The name is the plan's thesis in four letters: at this rung we are stuffing a
corpus far too small into a model far too large, on a deadline, and the expected
outcome is memorization with a thin margin of real learning. It stays honest as
the corpus grows — cramming *is* what training is — and it keeps the register of
a project that names its model tiers after bad writing. (`kibitz` is held in
reserve for an eval/judge layer if it ever wants its own name.)

### The crates

- **`kvetch-vocab/`** — in-workspace, `no_std` + alloc, zero deps. BPE encode/
  decode + the frozen vocab artifact. A `cli` feature (host-only) carries the BPE
  *trainer*. Vocab changes are wire-format changes; the freeze law lives here, and
  it is **ladder-wide** — every rung shares this crate's output exactly.
- **`kvetch-model/`** — in-workspace, `no_std` + alloc, zero deps. The transformer
  *forward pass* in plain Rust, checkpoint load, and the **`Rung` registry**
  (`Rung::{Drivel, Quip, Cliche, Ballad, Saga}` → `ModelConfig`, pure data,
  host-tested). Direct ancestor of kvetch's int8 kernels; the eval harness and any
  future itest link this and never a training framework.
- **`cram/`** — **excluded from the workspace** (the `learning/` precedent in
  the root `Cargo.toml`). `candle` + Metal backend; the training loop, autodiff,
  and checkpoint export. Takes a `Rung` + a corpus, emits a checkpoint. Excluded so
  `cargo xtask test` never compiles candle and a framework bump cannot break the
  gate.
- **`cram-corpus/`** — in-workspace, host-only. Corpus assembly, the
  deterministic split, augmentation, the validator funnel, the per-batch report.
  Depends on `stitch` (parse + type-check) and `babble` (Tier-0 generation).
- **Checkpoints are artifacts, not source.** Not committed, except one tiny drivel
  checkpoint enshrined as the deterministic eval fixture (the `panic-now` pattern
  the ladder doc names: feasibility artifact → permanent regression guard).

### The one abstraction the ladder actually needs

```
trait Rung {
    fn next_token_distribution(&self, prefix: &str, legal: &TokenSet) -> Distribution;
}
```

babble implements it from its bias tables; every trained rung implements it from
masked-and-renormalized logits. That single trait buys three things at once:

- **The eval harness scores every rung through one code path**, so babble's floor
  row and drivel's row are produced by the same code — they cannot become
  incomparable through drift. "babble is rung 0 of the same ladder" stops being a
  doc claim and becomes a compiled one.
- **Speculative decoding falls out later** as a pair of `Rung`s (quip drafts,
  ballad verifies) rather than a special case.
- **The grammar mask applies uniformly**, which is the ladder doc's soundness
  requirement for spec-decode anyway.

### Left open on purpose: the weight representation

drivel is f32; ballad and saga are int8 with int32 accumulators and fixed-point
softmax/RMSNorm. `kvetch-model` must not *preclude* that, but must not carry it
now either — a dtype parameter with one inhabitant is a fiction. The commitment
made here is narrower: **keep the forward pass generic over a `Weights` accessor
rather than indexing `&[f32]` directly**, so int8 arrives as a second impl rather
than a rewrite. Nothing more is designed until a rung needs it.

### Why candle, and the one risk it carries

candle over burn/dfdx/tch: it is the closest thing to "llama2.c in Rust", has a
Metal backend for the M1 Max, and its model code reads like the fixed-point
kernels kvetch will eventually need. burn is more ergonomic and heavier; tch is
PyTorch wearing a Rust hat, which defeats the point.

The risk is real and named: **candle's training path is less battle-tested than
PyTorch, so a loss curve that refuses to descend has two suspects instead of
one.** Increment 4 exists solely to eliminate the rig as a suspect before any
real data is involved.

## Non-goals (explicitly later)

Quantization and the int8 kernels; the on-target kvetch runner and weight
delivery via RAMfs/`MapAnon`; multi-hart matmul; speculative decoding; the KV
cache and the versioned-buffer protocol; Tier-1b/Tier-2 corpus generation (the
open-weight bulk run); FIM-ratio and vocab-size ablations; any rung above drivel.
Every one of those is well-specified in the design docs and none is needed to
answer "does 1M beat 0M".

---

## Increment 1 — the vocab, and its freeze

**RED** (`kvetch-vocab` tests): encode→decode roundtrips every program in the
real corpus byte-identically; every Stitch keyword and operator lexes to a single
token (a keyword split across merges would make the grammar harder to learn for
no benefit); the trained vocab's content hash matches a pinned constant — the
freeze, asserted, so a casual retrain fails the gate rather than silently
invalidating a checkpoint.

**GREEN**: BPE encode/decode in `no_std`; the trainer behind the `cli` feature;
the vocab embedded as data.

**Note the budget squeeze at this rung**: a 4K vocab with `d_model = 128` puts
~half of a 1M budget in the embedding table. Tie input/output embeddings and lean
toward the small end of the ladder's 2–4K range. Record the decision — the ladder
inherits it and cannot revisit it casually.

## Increment 2 — corpus assembly and the held-out split

**RED** (`cram-corpus` tests): the split is deterministic given a seed and disjoint by
*program*; no held-out program survives alpha-normalized MinHash against any
training program (including its augmentations — the leak this test exists to
catch); token counts are reported per source (`fs-image/`, `stitch/src/prelude.st`,
test fixtures, canon); re-running produces byte-identical splits.

**GREEN**: source walker, the validator funnel (parse → type-check → dedup) from
the bootstrap's Stage 0, augmentation passes, and a machine-readable per-batch
report. This is [babble.md](babble.md)'s deferred increment 9, unblocked — the
summary format is now being built against a real harness rather than guessed at,
which is exactly why it was deferred.

## Increment 3 — the eval harness, and babble's floor row

**RED**: `score(model, held_out) -> Report` where `Report` carries masked NLL,
`unconstrained_parse_pct`, FIM match, and shape distance; **babble's row is
computable with no trained model in existence** and is pinned as the floor.
A uniform-over-legal control (babble with flat tables) scores strictly worse than
babble with its tuned tables — the test that proves the harness can detect the
signal it exists to measure, before any real model is on the line.

**GREEN**: the harness, plus the one new babble API it needs — **`p(class | prefix)`
over the legal set, not just `pick`**. `pick` must be shown to be a draw from
exactly that distribution (one test pinning the two together, the same
anti-drift discipline `admits_next`/`valid_next` already use in the oracle).

## Increment 4 — the rig can learn at all

**RED** (`cram`, host-only, not in the gate): training 8 fixed sequences
for N steps drives loss below a threshold near zero. The classic overfit-one-batch
sanity check, promoted to the rig's unit test. A rig that cannot memorize 8
sequences is broken, and this is the only increment where that is cheap to tell.

Second RED: a checkpoint exported by `cram` and loaded by `kvetch-model`
produces **identical logits** for a fixed input. The two implementations of the
forward pass — candle's and ours — must agree, or every later number is measuring
the wrong model. This test is ladder-wide: it is the same assertion that will
later guard int8 export against the f32 reference.

**GREEN**: the `Rung` registry, training loop, checkpoint format, the
`kvetch-model` forward pass.

## Increment 5 — the grammar-learnability probe (drivel-on-babble)

**RED**: train on ~1M tokens of babble output (free, unlimited, 100% valid) and
measure **unconstrained** `parse%` on generated samples. Assert it clears a floor
that demonstrates real grammar acquisition rather than noise (pin the threshold
when first measured — this is a characterisation increment, so record the number
and gate against regression, don't guess it now).

This increment deliberately **cannot** beat babble at anything: babble is the
teacher, so its ceiling is the teacher. Its value is a clean answer to "can a
model this small learn Stitch's grammar *when data is not the constraint*" —
which, if the answer is no, tells us increment 6 was never going to work and
saves the corpus effort. Decoupling the two failure modes is the whole point.

**It is also the cheapest ladder-wide experiment we will ever have.** Because the
teacher generates unlimited valid data for free, the same probe run at
`Rung::{Drivel, Quip, Cliche, Ballad}` yields a **grammar-acquisition curve
against parameter count** — measured, on our own grammar, at ~$0 and no corpus
dependency. If drivel is already near-perfect the curve is uninformative and the
finding is "grammar is not what parameters buy here", which is itself worth
knowing before spending on corpus. Run drivel now; the upper rungs are a follow-up
that needs nothing but time.

## Increment 6 — the tracer: drivel-on-real beats babble

**RED**: train on the real corpus (repeated + augmented, ≤4 epochs) across the
{0.25, 0.5, 1}M sweep; assert the best checkpoint's **held-out masked NLL is
strictly below babble's floor row** from increment 3. Report the full table —
every size, every metric, including the ones that are not the gate.

**GREEN**: whatever the sweep shakes out. Expect memorization; expect a small
margin; a small margin is the success criterion, stated up front so it cannot be
retroactively talked up or down.

**If it loses**, the diagnosis is already instrumented: increment 5 separates
"can't learn the grammar" from "not enough data", the funnel separates corpus
problems by stage, and the flat-tables control in increment 3 proves the harness
can see signal. A loss here is a measurement, not a dead end.

## Increment 7 — the checkpoint manifest

**RED**: a checkpoint's manifest records `{name, params, vocab_version,
grammar_hash, corpus_version, eval_scores, trained_at}`; a checkpoint whose
`grammar_hash` differs from the current parser's is reported **stale**. The same
drift-check philosophy `docs/generated/` already runs, pointed at neural
artifacts.

**GREEN**: the manifest type + an xtask verb printing the ladder with staleness
flagged. One rung today; the shape is what the ladder inherits.

---

## Gate

`cargo xtask test && cargo xtask itest && cargo xtask itest --scramble`, plus
`cargo xtask clippy`. `kvetch-vocab`, `kvetch-model`, and `cram-corpus` are
ordinary workspace crates and join the gate normally. **`cram` is excluded from
the workspace and is not gated** — it is run by hand, its output is an artifact,
and the pinned eval fixture is what protects the gate from it.

Mutants over `kvetch-vocab` (encode/decode is exactly the "silently wrong on one
branch" shape mutation testing is for) and over the `cram-corpus` split/dedup
logic (a leak that survives here poisons every number the plan produces).

## The eval-floor artifact

Increment 3's babble row and increment 6's drivel table are recorded together as
the ladder's first two rows — the chance-level floor and the first rung above it.
Every later rung is measured against this file, per the ladder doc.
