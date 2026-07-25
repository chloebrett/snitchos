# The generative ladder: kvetch's model tiers, bootstrap, and lifecycle

**Status:** 📐 **DESIGN — exploration, not started.** Companion to
[llm-design.md](llm-design.md) (architecture, corpus synthesis, verification,
provenance). This doc owns the *models themselves*: the named tier ladder, the
tracer-bullet bootstrap, the quantitative gates, speculative decoding, and the
retrain-as-CI lifecycle that keeps the ladder fresh as Stitch evolves.

---

## The ladder

Six generative rungs, one shared tokenizer/vocab, one corpus. The naming
register is **the taxonomy of bad writing**, escalating from noise to competent
forgery — self-deprecating on the surface, and mostly an honest technical
description underneath.

| Rung | Name | Params | Character | Home |
|---|---|---|---|---|
| 0 | **babble** | 0 | Uniform-random walk over the continuation oracle's valid-token set. No weights at all | Everywhere — it's a code path, not a checkpoint |
| 1 | **drivel** | ~1M | Real words, meaning optional. The floor of actual language | Scaling-curve anchor; cheapest CI fixture |
| 2 | **quip** | ~3M | Short; occasionally lands. The tracer bullet | Tracer; speculative-decode draft |
| 3 | **cliché** (`cliche` in code) | ~10M | Technically correct, wholly unoriginal — pure formula | Keystroke-latency line completion on the VF2 |
| 4 | **ballad** | ~30M | First rung that carries a narrative: a whole function with a beginning, middle, end | **The product.** VF2 sweet spot (~20–50 tok/s int8) |
| 5 | **saga** | ~100M | The epic | Host / browser (snemu-wasm) / relay tier only — ~6–15 tok/s on the board is not interactive |

`cliché` at rung 3 is a product-requirements statement wearing a joke: an
autocompleter's entire job *is* the cliché — the predictable idiom, the
expected next thing. Nobody wants creativity at the cursor.

**Off-ladder: `verdict`** — the ~1M *classifier* (help-card routing: context
bundle → label). Different species entirely: different objective, different
data ((telemetry bundle, correct card) pairs, bootstrappable by frontier
labeling), and plausibly not a transformer at all at first — the input is
largely structured (frame types, fault kinds, cap states), so
gradient-boosted trees or hand rules over those features may embarrass a 1M
LM. It shares the manifest/recency machinery below and nothing else. A model
whose entire output is one judgment, named accordingly.

### babble is two components in one

Full design: [babble-design.md](babble-design.md) (oracle API, bias
tables, kvetch protocol v0, eval floor, increments).

Uniform-random-over-valid-set is the **Tier-0 grammar sampler wearing the
serving hat**: batch mode generates corpus, streaming mode is "model zero"
behind the kvetch endpoint. Build it once (with the same mild depth/length
probability biasing both jobs need — CSmith-style tables; pure uniform either
terminates instantly or nests pathologically). What it buys:

- **Full serving-path bring-up with zero ML**: oracle → BPE mask → endpoint →
  stim ghost text → itest scenario, all before a single weight exists. It
  needs no matmul, so endpoint/protocol/stim wiring can precede kvetch's
  fixed-point kernels entirely.
- **The null baseline every eval needs**: babble pins chance level on every
  metric (FIM test-pass ≈ 0%, idiom-match ≈ 0%), so each rung's improvement is
  *measured*, not assumed. Every trained model is definitionally an
  improvement over babble — and now provably so.

## The bootstrap

Principles: **closed models judge, open models generate, humans polish**; and
**every stage gate is a measurement, not a vibe**.

| Stage | What | Gate to proceed |
|---|---|---|
| 0 | Validator harness (parse → type → tests → alpha-normalized MinHash dedup) + production-coverage counter + recipe-tuple generator. No model calls | Harness emits the per-batch report (below) |
| 1 | Gold exemplars: ~10 programs, frontier-drafted, **hand-polished**; mine `fs-image/stim/stim.st` excerpts | "You'd sign every line" |
| 2 | Local pilot: 27–32B open model, overnight batches, ~1M candidate tokens; iterate recipes by day | Yield ≥ ~40%, coverage still climbing |
| 2.5 | **Tracer bullet**: at ≥500K validated tokens, train **quip** end-to-end — tokenizer, FIM, train, quantize, export, kvetch loads it, decodes under snemu | The whole pipe is green; unconstrained-parse% ≫ babble |
| 3 | Volume seed (~1M tok) via open-weight frontier (K3-class, hosted) — **after reading its license** | License permits training + publishing the corpus |
| 4 | Frontier-judge idiom drift on samples; then rented-GPU bulk run (same 30B, vLLM, ~30M tok in hours) | Post-run coverage/dedup match the pilot (pin sampling params across local/rented) |
| 5 | Mix (logged ratios; Tier-0 capped ~25%; real + canon corpus heavily up-weighted) and train the ladder | Eval gates vs previous checkpoints |

### The per-batch report (machine-readable, comparable across weeks)

- **Yield as a funnel, never one number**: parse% → type% → test% →
  dedup-survival%. The failure *stage* is the diagnosis: parse deaths = model
  doesn't know the grammar (exemplar problem); type deaths = knows shape, not
  semantics; dedup deaths = diversity problem.
- **Production coverage** and its *curve vs candidates generated* — plateau
  means that recipe axis is mined out.
- **Per-recipe yield and per-recipe dedup rate** (great yield + dup factory is
  a real failure mode).
- **Distribution-vs-real deltas**: shape statistics (nesting depth, match
  arity, function length) and identifier entropy against the real corpus —
  early warning for "legal but nothing like Stitch-as-written".

### The canon stratum

Between gold and bulk sits the **canon**: a few hundred programs snitchos
*actually wants* — FS utilities, telemetry consumers, supervise demos, tour
examples. Validated **via use** (shipped in `fs-image/`, run in itests,
anchoring tour chapters — continuously re-validated by the whole gate), it
fixes the real corpus's genre skew (currently ~45% one FSM-heavy program),
and every canon program is simultaneously training data, userland ecosystem,
documentation example, and regression fixture. Up-weight it with the
hand-written corpus in the training mix: total real Stitch today is ~2K lines
≈ 20K tokens (≈0.05% of target volume) — repeat it heavily; it's the only
"how Stitch is actually used here" signal the bulk tier merely imitates.

## Comments: invisible below ballad (decided 2026-07-25)

Stitch's lexer *skips* comments (`//`, nestable `/* */`), so they never become
tokens: they are legal at any token boundary, invisible to the grammar, and
the continuation oracle needs no change for them. The decision is entirely
about the **model**, and it is a vocabulary decision.

Including comments means learning **two distributions at once** — code
(low-entropy, highly structured, where the grammar mask does most of the work)
and prose (high-entropy, where the mask does nothing). At 10–30M params that
degrades both: ~30M is roughly the floor for coherent simple English *alone*
(TinyStories), and the vocab cost is lopsided — comments might be 10–20% of
tokens while demanding most of a 2–4K vocab, or else English shreds into byte
fragments and inflates corpus size for content the model cannot use.

The honest cost: **comment→code is the most valuable completion scenario in
real editors** (docstring above, body below). Excluding comments forecloses it
until a bigger rung.

### The policy is uniform across the ladder — per-rung differences break spec decode

A first draft of this section varied the policy by rung (invisible below
ballad, input-only at ballad). That is wrong. Vocabulary uniformity is already
law, so comment tokens exist for every rung or none; the subtler failure is
that **per-rung *training* differences do not break speculative decoding's
correctness — they destroy its economics.** The acceptance test stays valid
whatever the draft was trained on (the output distribution is still exactly
the target's); what collapses is the acceptance *rate*, and it collapses
precisely in buffers where a docstring carries signal — the completions that
matter most. One policy, every rung.

### Input-only, permanently — the argument is verification, not capacity

The quality mechanism of this whole arc is generate → verify: parse,
typecheck, run the tests, check authority against the kernel oracle.
**Comments are the one artifact that stack cannot check.** A confidently wrong
comment passes every gate we have; everything else the model emits is
falsifiable, prose is not. So "read but never author" is not a scale-driven
compromise to revisit at saga — it is the same rule as the rest of the design:
do not emit what you cannot verify. It also composes with provenance, since
model-written prose explaining model-written code is the highest-trust,
lowest-verification artifact the system could produce.

**Input-only is a training decision, not a decode mask** — this is the crux:

- comments appear **in the context window**;
- comment tokens are **masked out of the loss**: the model is never trained to
  predict them;
- at decode the mask never admits them (free — constrained decoding already
  does exactly this, which most stacks cannot cleanly do).

The loss mask is what makes this a capacity *saving*. Without it, "input-only"
is merely a decode restriction and the prose tax has already been paid in
training. With it, the model learns to *condition* on documentation without
spending parameters learning to *produce* it — and because the policy is
uniform, spec decode is unaffected: both models condition identically, neither
proposes comment tokens.

This fully supports the valuable scenario: docstring above, body below is FIM
with the comment as context. Generating comments was always the less valuable
direction.

### What owning the corpus buys, and where it stops

Real Stitch comments are *discursive* — `fs-image/stim/stim.st` opens with
multi-paragraph essays using rare vocabulary (`operator-pending`, `charwise`),
cross-file references and nested parentheticals. Nothing on this ladder will
learn to write those, which is the empirical case for never trying.

We can define **synthetic** comments as a controlled sublanguage (small
vocabulary, template shapes, validated by a micro-grammar — the
generate→validate loop applied to documentation, with Tier-2 prompts
instructed and non-conforming programs rejected). But **we do not own the
user's future comments**: train on a tidy sublanguage, meet `stim.st` at
inference, and the distribution mismatch has moved rather than disappeared.
Two things genuinely help:

1. **Normalize on input.** Because comments are never reproduced, kvetch may
   preprocess them — first sentence only, drop rare tokens, truncate — mapping
   real prose *toward* the training distribution. This option exists only
   because we chose input-only.
2. **Own the convention, not just the corpus.** If Stitch adopts a doc-comment
   convention (`///` attached to declarations) and the canon stratum plus the
   user docs follow it, real code converges toward the learnable form. That is
   corpus ownership at full strength — house style, not just synthetic data —
   and it is available here in a way it is not for a language with existing
   users.

**Doc comments only, not free-floating ones**: positionally structured, shorter,
more formulaic, and exactly where the signal is. `stim.st`'s file-header essays
would be excluded from training context; its per-declaration comments kept.

babble is the one exception, and only because it has no vocabulary at all: it
may emit comments behind a flag — free, and good devlog material — but the flag
must be **off** for Tier-0 corpus generation.

### The simple-register idea (open, and time-sensitive for the vocab freeze)

Rewrite the corpus's comments in **TinyStories register** — the ~1500 simple
English words a small child knows — as a *derived* corpus, not a source
rewrite. Four reasons it is more than a joke:

- **It is the one prose regime with direct evidence at this scale.** TinyStories'
  result is precisely that this vocabulary is coherently learnable at 10–30M
  params. Every other register we might pick is a guess; this one is measured,
  and by us.
- **The register is the provenance marker.** The hazard with generated comments
  is that prose is trusted most and verified least — but picture-book register
  *announces itself*. Nobody mistakes "this bit takes the list and makes it
  bigger" for a human's `// operator-pending state for r`. In-band, legible at
  a glance, not forgeable by accident.
- **A controlled vocabulary is machine-checkable.** "Write good comments" is not
  a validator; "use only these words" is set membership. It drops straight into
  the generate→validate pipeline as a rejection rule at no cost.
- *(Considered and rejected: initialising from the existing 30M TinyStories
  checkpoint. The ladder is built from scratch in candle — see
  [../plans/drivel.md](../plans/drivel.md) — so inheriting that architecture is
  not wanted. The register idea stands on its own; it does not need the
  transfer, and the vocabulary therefore carries no freeze-time urgency beyond
  covering whichever register the corpus adopts.)*

It also answers the distribution-mismatch problem that input-normalization only
half-solved: rather than normalizing real prose at inference, **transform it at
training**, and teach the model the single register it can actually reach.
Applied as a transform it extends to Stitch whose comments we did not author.

**Where this direction went:** once a comment-free model works, the register
idea is best pursued as an **off-ladder sibling captioner**, not as a mode of
the code model — which dissolves the vocabulary, capacity, per-rung-policy and
spec-decode problems in one move, and admits a real check
(execution-based back-translation). Full design:
[prattle-design.md](prattle-design.md).

**What it does not fix:** verification. "Makes the list bigger" attached to a
function that shrinks it passes every gate we have — simple prose is *feasible*
prose, not *checkable* prose. So input-only remains the shipping default; this
is the one credible path to generation, and it costs nothing to keep open.

**Do not apply it to real sources.** `stim.st`'s comments are genuinely good
technical writing — the why, the cross-references, the rationale — and are part
of the project's pedagogical value. The simple-register twin is a build
artifact derived from them, never a replacement for them.

**Drop rationale comments from the transform — keeping them trains
fabrication.** Simple vocabulary makes prose *fluent*, not *grounded*. A
comment like "operator-pending stays within Normal mode, so the cursor still
shows" states a design consequence that **is not present in the code**: no
model of any size recovers it from the declaration, because it was never in
the training signal. Transform such a comment and the corpus teaches the model
to produce rationale-*shaped* assertions it cannot possibly ground — confident
invention, in the most authoritative-sounding register in the file. So the
transform keeps **signature-adjacent** comments and drops rationale ones, and
the earlier observation that cross-references and design intent "die in the
transform" is the transform working, not a loss.

The general form of this is worth stating: **the valuable comments are the
unlearnable ones, and the learnable ones mostly restate the signature.** That
is a third independent argument for input-only, after verification and
capacity.

### When the user's comment is not in the register

The realistic case, and it needs a deterministic answer. It cannot break
correctness — grammar-constrained decoding still guarantees legal Stitch, so
the mask is a floor on quality. What degrades is conditioning: an
out-of-register comment shreds into many subword or byte pieces (a 20-word
technical comment can cost 80+ tokens), those tokens carry barely-trained
embeddings, and the KV cache fills with low-information context — paid twice on
a bandwidth-bound board. Note this forces a vocab decision: **byte-fallback is
effectively mandatory**, or the tokenizer fails outright on unexpected input.

**The fix is a `<comment>` sentinel.** A comment that fails the whitelist is
replaced, before sampling, by one opaque token:

- fixed cost regardless of length — no context bloat;
- honest — the model learns "documentation here that I cannot read", instead of
  pretending to understand fragments;
- preserves the structural signal that a declaration *is* documented, which is
  informative on its own;
- deterministic and model-free at inference (set membership) — unlike
  "normalize the prose", which quietly required a model to do the simplifying.

**The corpus must contain sentinels too** — sentinel-ise a fraction during the
transform, or the token is itself out-of-distribution the first time a user
hits it. The same mechanism covers non-English comments, ASCII-art headers, and
**commented-out code** (which is not prose at all, and would otherwise tempt
the model to suggest reinstating whatever was disabled).

**Make it measurable:** emit `kvetch.comments_elided_total`. If real usage
elides most comments, the register choice was wrong — and that arrives as a
number on a dashboard rather than a slow mystery about why completions feel
worse in well-documented files.

## One vocab, one scaling curve, one draft

**The vocab freeze is law.** Every rung shares tokenizer + vocab exactly.
Changing the vocab invalidates the *entire ladder* (and speculative decoding
with it) — treat vocab changes like wire-format changes: versioned in the
manifest, never casual.

**The ladder is our own scaling law.** {1, 3, 10, 30}M trained on identical
data/vocab are four points on a capability curve nobody has published (a
single-language, validated-corpus regime this small). drivel anchors the
bottom; the knee's location decides how small the shipped model can be.

**The ladder tops out at saga — above it, change species, don't scale.**
A 300M/1B from-scratch rung is *not* blocked by training cost (we're
data-limited; even 1B ≈ 1e18 FLOPs ≈ hours on an A100, <$10). It's blocked by
two other things: (a) the corpus can't feed it — 1B against ~40M tokens is
~100× over-parameterized, and past the corpus's information content extra
params buy memorization of the synthetic programs, not capability; (b) on the
host, the competition isn't a bigger from-scratch model but a **fine-tuned
open one** — every ladder model knows *only Stitch* (no English, no
world-code priors; it completes, it cannot explain), while a LoRA'd open
4–8B-coder brings English + general code understanding + our corpus, and wins
at everything the host tier is for (help synthesis, explanation, agent
planning). So: **from-scratch below saga, fine-tune above it.** A 300M probe
stays on the menu only if the sweep shows the curve still climbing at saga
(a ~$3 experiment); 1B is struck.

**Speculative decoding** (the shared vocab's payoff): quip drafts k tokens,
ballad verifies them in one batched pass. On the VF2 ballad is
*bandwidth-bound*, so verification reads the 30M weights once for k tokens —
accepted drafts amortize exactly the scarce resource. Code is the
highest-acceptance domain there is; expect 1.5–2.5× decode on the board from
models we were training anyway. Composes with the grammar mask (apply it to
both draft and verify; renormalized acceptance stays sound), and
singleton-mask positions (forced tokens) skip both models entirely.
Grammar-constrained speculative decoding on embedded scalar hardware:
plausibly a first. (babble cannot draft — spec decode's win is draft-target
*agreement*, and uniform-random agrees with nothing.)

## Lifecycle: retraining is CI, not an event

Stitch will evolve (audio library, display library, syntax changes); the
ladder must never be more than one push-button run behind.

- **The corpus is a derived artifact** (like `docs/generated/`): what's
  pinned is generators + recipes + seeds + validators. On a language change,
  re-validate the whole corpus — and **"this change broke 14% of the corpus"
  is a language-design signal**, a compile-the-world test for Stitch. Broken
  strata are auto-migrated where mechanical (we own the AST — write the
  `old → new` transform) or regenerated where not. Library additions are
  incremental: new exemplars + new recipes + one targeted stratum.
- **Constrained decoding is the staleness airbag**: the mask comes from the
  *current* parser, so a stale model physically cannot emit removed syntax —
  it merely doesn't know new idioms yet. Grammar changes propagate instantly
  via the oracle; only idiom knowledge waits for retrain.
- **Checkpoint manifest**: `{name, params, vocab_version, grammar_hash,
  corpus_version, eval_scores, trained_at}`. An xtask verb prints the ladder
  with staleness flagged (checkpoint grammar hash ≠ current parser = stale).
  Drift-check philosophy applied to neural artifacts.
- **Eval gates**: unconstrained-parse% (the training-side yield analogue —
  meaningful from quip onward), held-out FIM pass rate via the validators,
  and *no regression vs the previous checkpoint*. babble is the fixed floor;
  quip's tracer checkpoint is enshrined as the deterministic CI fixture for
  kvetch itests (the `panic-now` pattern: feasibility artifact → permanent
  regression guard).
- **Fleet, not cluster**: rungs are independent jobs. The M1 Max trains one
  while the 7700 XT (Linux ROCm — worth one weekend experiment, never a
  critical-path dependency) trains another; **"retrain the entire ladder
  overnight" is one machine per rung**, or ~$5 of cloud for all rungs (our
  corpus is data-limited, not Chinchilla-limited: saga is ~1e17 FLOPs ≈ 40
  min on an A100 ≈ $1–2; every rung below is under $1 or an hour locally).
  Heterogeneous *single-model* training across Metal+ROCm: interesting
  research, terrible dependency — not doing it.

## Deferred: editor-command / next-edit prediction

Flagged for later, decided in outline (2026-07-25). The idea: predictions in
stim's *command* vocabulary (`ciw` → replacement), not just inserted text —
next-edit prediction à la Zed's Zeta / Cursor Tab, but with the editor's
modal grammar in the loop. Findings from the design discussion:

- **The decided shape, if/when built: one semantic-edit intelligence,
  commands as a compilation target.** A mode-factored split (action model in
  normal mode, content model in insert) was considered and **rejected**: the
  interesting edits fuse location+transformation+content ("replace this word
  with that one"), so choosing the action requires ~all of the content
  understanding — the mode boundary is a UI joint, not a cognitive joint, and
  the split duplicates intelligence. Instead: ballad-scale trunk,
  mixture-trained (FIM + next-edit objectives; insert-completion is the edit
  whose region is empty), predicting compact semantic edits conditioned on
  edit history. A deterministic **golf-solver compiles predicted edits into
  stim command sequences** for preview/provenance/tutoring ("model proposed
  `ciw`") — always canonical-idiomatic, never model-fumbled. Training data is
  native (before, after) pairs (git history, the stim edit log, synthetic
  Stitch mutations) — no labeling pass needed; the solver's inference-time
  rendering job replaces its training-time labeling job.
- **The FSM routes queries, not models**: normal mode asks for edit
  predictions, insert mode for FIM ghost text — same trunk, two prompts.
- **No vocab reservation now.** Cheap push-button retraining is precisely
  what makes early vocab insurance unnecessary — a future edit-format needs
  only delimiter tokens, and bumping the vocab version costs one overnight
  fleet retrain. (The freeze law's content is coordination *across* the
  ladder at any moment, not permanence over time.)
- **First iteration is the insert/FIM model only** — conceptually simplest
  and ~90% of the pipeline; edit-prediction lands later as an additive
  mixture objective + one retrain.
- **The golf-solver is its own stim milestone first — a deterministic vim
  coach, zero ML.** Pure function `(before, after, actual_commands) →
  verdict`: segment the edit log into bursts (by thinking pauses), compare
  actual against better sequences, teach via end-of-session digest or replay
  mode (another fold over the edit log; never mid-flight). Cost model is
  *idiomatic* cost, not golf cost — start as a peephole rule library
  ("`xxxxx` → `5x`/`daw`", each rule carrying its explanation), graduate to
  bounded search later. Every (actual, optimal) pair it computes is exactly
  the future edit-model's supervision — the solver starts as coach, matures
  into labeler, ends as compiler: three roles, one component, each funding
  the next. The far-future growth of the coach — competence estimation,
  passive spaced repetition over organic occurrences, FSRS-class memory
  models trained on the user's own edit log, competence-gated
  crutch-masking — is its own design (and plausibly its own paper):
  [stim-tutor-design.md](stim-tutor-design.md). V1 here remains the
  peephole digest.
- The fully unified *session-stream* model (predict the next event the editor
  receives, commands and content interleaved) remains the research-flavored
  horizon; nobody in the Zeta/NEP lineage models the editing *process*, and
  we own editor, grammar, and model in one stack.

## Open questions

- Vocab design: BPE size (~2–4K), identifier word-piece treatment — decided
  once at quip time, then frozen. What goes in the freeze-break checklist?
  The vocabulary must cover whichever comment register the corpus adopts (see
  the simple-register section) — but no transfer-learning constraint applies,
  since the ladder is trained from scratch.
- Where the knee of the scaling curve sits (the {1,3,10,30}M sweep answers
  this — don't pre-commit ballad as the ship rung until measured).
- Spec-decode pairing: quip→ballad assumed; is cliché→saga worth it on host?
- Does verdict start as trees/rules over structured features, with the 1M LM
  as a later upgrade only if free-text fragments dominate routing accuracy?
- Canon stratum curation: who decides what's canon, and does canon-ness live
  in `fs-image/` placement or a manifest?
