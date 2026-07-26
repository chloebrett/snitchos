# Corpus MVP — get a local model writing real Stitch

**Status: 📐 PLAN — not started.** A deliberate thin vertical slice through
Stages 0–2 of the bootstrap, optimised for *seeing an LM emit semantic Stitch
this week*, not for corpus quality.

Related: [../docs/generative-ladder.md](../docs/generative-ladder.md) (the full
bootstrap table this shortcuts), [stage-0-validator-funnel.md](stage-0-validator-funnel.md)
(the complete Stage 0 — this borrows its funnel and skips its measurement
suite), [../docs/llm-design.md](../docs/llm-design.md) (corpus tiers, the
diversity axes), [babble.md](babble.md) (Tier-0, the null baseline),
[drivel.md](drivel.md) (the rung that consumes the output).

---

## The goal

Point a ~4B local open model at the existing ~20k-token real-Stitch corpus, feed
it recipe tuples, and let it run until **~500k validated tokens** exist. Then
train drivel on that and compare against the babble-trained drivel.

The output does not need to be good. It needs to be **semantic** — programs that
mean something, rather than grammatically-valid noise. That is the one property
babble structurally cannot have, and the entire reason this is worth doing before
the corpus pipeline is finished.

## What this borrows from Stage 0, and what it skips

**Get it working first, make it principled second.** The target is a model
emitting Stitch that parses and typechecks; the funnel analysis, the coverage
curve and the machine-readable report are refinements that land once there is
output to analyse.

Stage 0's thesis is *the funnel is the product* — the stage a candidate dies at
is the diagnosis. That survives here in its crudest form: **a `println!` naming
the death stage** is enough to steer the first week. Structured counters are
Increment 6, not a prerequisite.

**Borrowed:**

- The parse → type gate (Increment 1 — without it there is no signal at all).
- Death stage, printed.
- Recipe tagging on every candidate — free at write time, unreconstructable
  later, so do it even while the analysis is a `println!`.
- Exact dedup.

**Skipped, deliberately:**

- MinHash near-dedup (exact only — near-dupes will get through, and that is
  acceptable for a first corpus).
- Production-coverage curve, distribution-vs-real deltas, per-recipe yield
  *analysis* (tag now, analyse when it matters).
- The run/test validation stage. Parse + type matches the existing canon gate;
  execution adds failure modes (no `main`, needs fixtures) for little MVP value.
- The augmentation tier. It multiplies whatever it is given — wait until the
  funnel is trusted.

## What already exists

- **The gate chain**, as a test: `stitch/tests/canon.rs` runs `parse_program` →
  `lower_items_to_core` → `check_program` filtered to `Severity::Error`. All
  three functions are public (`stitch/src/{parser,lower,check}.rs`). This is an
  extraction, not new logic.
- **`cram-corpus`** owns assembly, the manifest/fingerprint cache, and
  `Layout::{Flat, Printed}`. The funnel's survivors feed it.
- **`cram-eval`** gives the comparison at the end: `Predictor` for held-out
  masked NLL, `Generator`/`parse_rate` for unconstrained parse rate.
- **The printer** (`stitch/src/print.rs`) — not needed for MVP, but it is what
  makes augmentation possible later.
- **`cargo xtask cram`** exists (`xtask-cram/src/{main,eval}.rs`). `sift` does
  not. Use that name anyway, so this converges with Stage 0 rather than forking.

---

## Increment 0 — the spike, before any harness

**Twenty prompts per model, run by hand, across two or three sizes (4B / 8B /
14B).** This is not a formality; it decides the shape of everything below, and
it is about an hour of work.

Measure four things:

1. **Single-stream decode throughput** (tok/s) per model on this machine.
2. **First-pass parse yield** with 2–3 exemplars in the prompt.
3. **Where the failures die** — parse or type.
4. **The floor check**: did *any* program parse? See Increment 7 — a single
   parsing program is sufficient evidence that per-token legality is high enough
   for constrained decoding to be confirming rather than puppeteering. Zero
   parsing programs across sixty attempts is the one result that should stop
   this plan.

**Choose on type-pass rate and on eyeballing ten programs**, not on parse rate.
Parse rate gets flattened to ~100% by Increment 7 regardless of model, so it
cannot discriminate; what you are shopping for is whether the output *means*
anything, and no benchmark answers that for a language the model has never seen.

**Two configuration gotchas that will otherwise cost you a day:**

- **Turn thinking mode off** (Qwen3 and similar ship it on). Thinking tokens are
  pure cost here — you discard them, and they can triple time-per-candidate.
- **Pin sampling parameters now and log them in the manifest.** The ladder doc's
  Stage 4 gate compares local-pilot coverage against a later bulk run; if
  temperature and top-p drift between them, that comparison is meaningless.

Then do the arithmetic, with *your* numbers rather than anyone's estimate:

```
candidate_tokens = 500_000 / yield
wall_clock       = candidate_tokens / throughput
```

Illustrative only, to show the shape of the decision: at 20% yield that is 2.5M
candidate tokens, which at 40 tok/s is ~17 hours and at 300 tok/s is ~2.3 hours.
**If the single-stream number puts a full run beyond overnight, build Increments
7 and 8 before the first real run rather than after** — 7 first, since it is the
larger lever and it also removes the wasted candidates that 8 would otherwise
have to carry. Measure, don't assume — that lesson is already paid for in this
repo.

**Set expectations honestly:** a 4B model shown a language absent from its
pretraining, with a handful of exemplars, will fail a lot at first. A first parse
yield in the 10–30% band is not a bug and not a verdict on the idea. The funnel
tells you the fix: parse deaths → exemplar problem; type deaths → the model has
the shape but not the semantics, which is tighter recipes and better exemplars.

---

## Napkin math

Structural parts first (these are solid). M1 Max has **~400 GB/s** memory
bandwidth; decode is bandwidth-bound because the weights are streamed once per
token. A 4B model at q4 is ~2.4 GB, so the ceiling is `400 / 2.4 ≈ 165 tok/s`,
and real Metal efficiency lands somewhere around 40–60% of that.

The soft parts are marked. **Yield is a total guess and it dominates.**

| Quantity | Estimate | Confidence |
|---|---|---|
| Decode, single stream | 50–90 tok/s | structural, good |
| Prefill | 200–800 tok/s | rough |
| Prompt size (system + 2–3 exemplars + recipe) | 700–1500 tok | yours to set |
| Output per candidate | ~300 tok | from the 20k corpus's shape |
| Parse+type yield | **10–40%** | **unknown — measure in Increment 0** |
| Batching speedup (batch 8–16) | 4–6× | soft; Metal batches worse than CUDA |

Per candidate, single stream: prefill ~1000 tok ≈ 2.5 s, decode ~300 tok ≈ 4.3 s
→ **~7 s**, or ~4.5 s with a warm prompt-prefix cache.

For **1M validated tokens** ≈ 3,300 programs at ~300 tok each:

| Setup | 20% yield | 40% yield |
|---|---|---|
| Single stream, no cache | ~32 h | ~16 h |
| Single stream, prefix cache | ~21 h | ~10 h |
| Batched ×5, prefix cache | **~4 h** | **~2 h** |

The plan's 500k target is half of each. So: **1M is an overnight run
single-stream, or an afternoon batched** — and 500k is comfortably an overnight
even in the worst column.

Three levers, in order of return on effort:

1. **Yield.** 20%→40% halves everything, and it is an *exemplar quality*
   problem — which is exactly why the ladder doc says gold exemplars are
   hand-polished. Polishing a handful of programs buys more wall-clock than any
   engineering below.
2. **Batching** (Increment 8). ~5×.
3. **Prompt-prefix caching.** ~1.5×, and nearly free — see Increment 4.

**Constrained decoding (Increment 7) collapses the yield term to ~100% on the
parse axis**, which makes it a larger lever than batching and reorders the table
above. Treat the yield column as "what you get before the mask."

---

## Model choice

**Default: Qwen3-4B.** Apache 2.0, strong code ability for the size, good
instruction-following, first-class GGUF and MLX support. Qwen2.5-Coder-7B is the
code-specialist alternative (~4.5 GB at q4, still fast here). Model releases move
faster than this document; the *criteria* below are what to re-apply.

**License is a selection criterion, not boilerplate** — the ladder doc's Stage 3
gate is "license permits training + publishing the corpus," and the provenance
paper depends on shipping it.

| Model | License | For a publishable corpus |
|---|---|---|
| Qwen3 / Qwen2.5-Coder | Apache 2.0 | clean |
| Phi-4-mini | MIT | clean |
| Gemma-3 | Gemma license | usable, but use restrictions propagate |
| Llama-3.2 | Llama license | restrictions + "Built with Llama" naming |

**Select for imitation, not for code benchmarks.** No model has seen Stitch, so
pretraining on Rust and Python transfers as code-shaped priors and little else.
What matters is (a) in-context imitation of unfamiliar syntax from 2–3 exemplars
and (b) constraint adherence — does it actually use the required constructs and
the three must-use words? Heavily code-tuned models are sometimes *worse* at (b),
preferring idiomatic Python to your spec. This is the doc's own "instruction-
following, not frontier taste."

**On going bigger.** The ladder doc targets 27–32B for the Stage 2 pilot.
Bandwidth-bound decode makes the trade explicit:

| Size (q4) | Weights | Realistic tok/s | × yield | Validated tok/s |
|---|---|---|---|---|
| 4B | 2.4 GB | ~80 | 10% | **8.0** |
| 14B | 8.5 GB | ~24 | 25% | 6.0 |
| 32B | 18 GB | ~11 | 40% | 4.4 |

Small wins on throughput unless the yield gap beats the ~7× speed gap, which
would need something like 70%-vs-10%. But **constrained decoding preferentially
rescues the small model** — its main weakness is exactly what the mask covers —
leaving semantic quality as the differentiator, and that appears in none of those
columns. The arithmetic cannot settle this; Increment 0 can.

**Keep the tooling decision reversible.** Pick a model available as both GGUF and
MLX weights (all Qwen3 sizes are), because Increment 7 forces a choice between
llama.cpp (GBNF grammars built in, static and approximate) and MLX (faster per
token on Apple silicon, custom logit processor calling `oracle.rs`, exact).

---

## Increments

### 1. The gate, as a library function

Extract the canon chain into something callable.

- **RED**: a canon program returns `Ok`; a syntax error returns `Parse`; a
  well-formed program with a type error returns `Type`. Gradual typing makes the
  third case a live question — `canon.rs` already ships a control test proving
  the type gate *can* fail, and this inherits that hazard.
- The verdict must carry the diagnostic text, not just the discriminant. That
  text is the raw material for repair traces later
  ([../docs/kvetch-rl-design.md](../docs/kvetch-rl-design.md) §5) and costs
  nothing to keep now.

**Salvage, don't discard — the gate has three exits, not two.**

Parse-failure and type-failure are different animals and deserve different
handling:

- **Type-failures are fully-formed, readable Stitch with a semantic bug.** Keep
  them as a logged stratum rather than dropping them; gradual typing already
  makes this gate a soft signal. That is the "a bit semantic" material this plan
  exists to get.
- **Parse-failures should be salvaged before they are dropped.** Truncate to the
  longest parsing prefix (the oracle gives valid-next sets per position, so the
  break point is computable), and auto-repair the trivial class — unbalanced
  delimiters at EOF is most of it: append the closers, re-run the gate. At 20%
  yield you are otherwise discarding 80% of the model's output, much of which is
  95% correct.
- **Log what survives neither.** A genuinely model-produced broken program plus
  its diagnostic is the scarcest input the RL branch has — §5's named risk is
  that reverse-corruption does not look like real confusion, and this *is* real
  confusion, arriving free as a byproduct. Re-prompting with the diagnostic
  yields an authentic repair episode. Even if nothing trains on them, do not
  throw them away.

**Do not train drivel on non-parsing text.** Keeping it is for later use; mixing
it into the training corpus teaches the model that invalid token sequences are
valid, which corrupts exactly the legal/illegal boundary that makes
constrained decoding work at all.

### 2. Candidate extraction

Model output → program text.

- **RED**: a fenced block is extracted cleanly; prose before and after is
  stripped; no fence means treat the whole response as the candidate; multiple
  fenced blocks means take the first and count the rest as a extraction-stage
  death (a distinct counter — it is a prompt problem, not a Stitch problem).

### 3. The recipe tuple

`domain × 2–3 required constructs × size bucket × shape × 3 must-use words`,
per [../docs/llm-design.md](../docs/llm-design.md).

- **RED**: same seed yields the same recipe sequence; the axes cross before
  repeating; must-use words are drawn from the real corpus's identifier
  distribution, not babble's 571.
- Keep it a plain data struct. The report keys on it.

### 4. The prompt builder

Exemplars + tuple → prompt.

- **RED**: the prompt names every required construct verbatim; the selected
  exemplars each use at least one of the tuple's required constructs.
- **Exemplar selection is the highest-leverage cheap trick here.** Index the 20k
  corpus by construct and pick exemplars that *match the recipe*, rather than
  sampling at random. It is the difference between the model guessing and the
  model copying, and it is a `HashMap` build at startup.
- **Structure the prompt for prefix caching**, which is worth ~1.5× on wall
  clock: invariant content (system prompt, exemplars) first, varying content
  (the recipe tuple) last. Interleaving them forfeits the cache entirely.
- Recipe-matched exemplars fight prefix caching, since the prefix changes per
  recipe. The cheap resolution: **bucket recipes by exemplar set and sort the
  work queue by bucket**, so consecutive calls share a prefix. This is free
  because Increment 3's sampler is seeded and deterministic — the whole recipe
  sequence exists before the first call, so it can be sorted.

### 5. The runner, single-stream

One candidate at a time against a local HTTP endpoint.

- **RED**: the runner is a trait; a fake responder returning canned text drives
  the whole pipeline end to end without a model. Every increment above must be
  testable with the model absent.
- Model choice is yours — a 4B-class instruct model with decent code ability.
  Whatever is already installed beats whatever benchmarks best.

### 6. Dedup and corpus append

Enough bookkeeping to accumulate a corpus; the *report* is Increment 9.

- **RED**: an exact duplicate is dropped and counted; survivors land in
  `cram-corpus` with their recipe tag attached; the death stage of every
  rejected candidate is printed.
- Exact dedup only. Near-dupes will get through and that is acceptable here.

### 7. Constrained decoding — the biggest lever

Mask the generator's logits with the continuation oracle so every candidate
parses by construction. Available *only* because the model is local and open:
logit access is the whole thing, and no hosted API offers it.

**This is rejection sampling with the rejection moved inside the loop.**
Unconstrained-plus-filter generates N candidates and keeps the survivors; the
mask does the same operation per *token*, so no compute is spent on a candidate
that was doomed at token 40. Same operation, done where it is cheap.

**Why the floor is low enough to be safe.** Program-level parse rate is an
exponentially amplified view of per-token legality — a program is ~300 decisions:

| Per-token legal mass | Program parse rate |
|---|---|
| 99.98% | 95% |
| 99.5% | **22%** |
| 99% | 5% |
| 95% | 1 in 3.4 million |
| 80% | ~10⁻³⁰ |

A 20%-yield model is already at ~99.5% per-token legality — it knows the grammar
and slips ~1.5 times per program. The failure mode where the mask drags an
ignorant model into syntactically valid nonsense needs per-token legality down
near 80–95%, which corresponds to a program parse rate of *zero*. Hence
Increment 0's floor check: **one parsing program is sufficient evidence to
proceed.**

Errors are not independent, though — real failures are bursty, where one wrong
step leads to fifty confident tokens in the wrong direction. That makes the mask
*more* valuable than the table implies (preventing the first wrong step prevents
the cascade) and it changes the instrument:

- **Log the mask's intervention rate — its distribution, not its mean.** How much
  probability mass did the model put on tokens the oracle rejected, per position?
  Low and scattered means the mask is confirming and the corpus is real. Clustered
  into long stretches means the model derailed and is being puppeted, and you are
  generating babble with extra steps. Same mean, opposite diagnosis.
- It is nearly free: the legal set is already computed to build the mask, so this
  is a sum over the rejected mass.

**Once constrained, parse rate stops being a metric** — it is 100% by
construction, exactly as it is for kvetch. **Type-pass rate becomes the real
yield number**, and Stage 0's distribution-vs-real deltas (identifier entropy,
nesting depth, function length) stop being nice-to-have: they are the primary
guard against high-effort babble.

- **RED**: a constrained run produces only parsing programs over a fixture
  batch; the intervention log is non-empty and its clustering is asserted on a
  synthetic case.
- **Reserve a deliberate unconstrained fraction** with a logged ratio. A model
  prevented from being confused produces no confusion to harvest, and Increment
  1's error corpus is the RL branch's scarcest input. Decide the ratio; do not
  discover later that the corpus has no failures in it.
- Implementation: GBNF (fast, static, approximates the grammar) or a custom
  logit processor calling `oracle.rs` (exact, needs a sampler hook). Either way
  there is a tokenizer-mismatch step — the oracle speaks Stitch token classes,
  the model speaks its own BPE — which is standard and solved but is real work,
  not a flag.

### 8. Batching — the parallel step

**Not "run N processes."** On an M1 Max, decode is memory-bandwidth-bound: the
weights are streamed per token, so N independent model instances contend for the
same bandwidth and buy you close to nothing. The win is **continuous batching
inside one process** — weights read once per step, amortised across the whole
batch.

- Concurrent requests against one server (llama.cpp server, MLX, or equivalent
  with continuous batching enabled).
- **RED**: a batched run and a single-stream run over the same seeded recipe
  sequence produce the same set of candidates. Determinism must survive
  batching, or every later comparison is unrepeatable.

### 9. The report and `cargo xtask cram sift`

The principled version, once there is output worth analysing.

- **RED**: a batch of fixtures produces asserted counts at each stage
  (extraction / parse / salvage / type / dedup); a recipe that yields only
  duplicates shows high yield and ~0% dedup survival.
- Report as a funnel, never as one number:
  `candidates → extracted% → parse% → salvaged% → type% → dedup-survival%`.
- CLI follows repo conventions (stream separation, `--json`; see the
  `cli-design` skill). Same name as Stage 0's, so the two converge rather than
  fork.

---

## The gate for this plan

500k validated tokens exist, and **drivel trained on them beats
babble-trained drivel** on:

- held-out masked NLL (`cram-eval`'s gate metric — compare against the
  uniform-over-legal floor at 2.758, not babble's 5.405), and
- unconstrained parse rate.

That is the ladder doc's Stage-2.5 tracer-bullet gate, pulled forward and run
with a smaller model. If it passes, the pipeline is real and the remaining work
is volume and quality. If it fails, the funnel says where.

---

## Not doing / open

- **More Tier-0 volume.** Cheapest lever, already spent: babble's lexicon
  saturates at 571 identifiers and that ceiling survived a 33× data increase.
- **The repair axis** — deferred to whenever the RL branch earns it
  ([../docs/kvetch-rl-design.md](../docs/kvetch-rl-design.md)). Recipe axes are
  additive, so nothing is lost by waiting, and a later axis draws its must-use
  words from a better identifier distribution than today's 20k.
- **License check before bulk.** Not blocking for a local MVP, but the corpus is
  meant to be publishable alongside the provenance paper, and the generating
  model's terms decide that. Settle it before any run that produces corpus you
  intend to keep.
- **Open: how much of the 20k to hold out.** Every token spent on eval is a
  token not available as an exemplar, and the held-out set is already thin
  enough that two close arms may not separate. Decide before Increment 0, not
  after seeing results.
