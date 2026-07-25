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

## One vocab, one scaling curve, one draft

**The vocab freeze is law.** Every rung shares tokenizer + vocab exactly.
Changing the vocab invalidates the *entire ladder* (and speculative decoding
with it) — treat vocab changes like wire-format changes: versioned in the
manifest, never casual.

**The ladder is our own scaling law.** {1, 3, 10, 30}M trained on identical
data/vocab are four points on a capability curve nobody has published (a
single-language, validated-corpus regime this small). drivel anchors the
bottom; the knee's location decides how small the shipped model can be.

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

## Open questions

- Vocab design: BPE size (~2–4K), identifier word-piece treatment — decided
  once at quip time, then frozen. What goes in the freeze-break checklist?
- Where the knee of the scaling curve sits (the {1,3,10,30}M sweep answers
  this — don't pre-commit ballad as the ship rung until measured).
- Spec-decode pairing: quip→ballad assumed; is cliché→saga worth it on host?
- Does verdict start as trees/rules over structured features, with the 1M LM
  as a later upgrade only if free-text fragments dominate routing accuracy?
- Canon stratum curation: who decides what's canon, and does canon-ness live
  in `fs-image/` placement or a manifest?
