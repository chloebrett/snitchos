# Plan: TinyNerd — buying the English half of the corpus instead of paying for it

**Status (2026-08-27)**: 📐 **Not started — design only, zero code.** Steps 1–3 are
cheap probes that gate everything after them; do not generate a corpus before step 3
returns a number.

## Goal

Stop spending ~46% of the ladder's training budget teaching a 1M–3M model an English
it demonstrably cannot learn, without deleting the English — by pretraining on a
purpose-built simple-technical-prose corpus ("TinyStories, but a nerd") and
fine-tuning on Stitch.

## Context

Two facts from the notes, and one that this plan adds.

**Half the budget goes on English.** [batch9-findings](../notes/batch9-findings.md)
measured 44.2% of batch9's bytes and **47.3% of its tokens** as comment text, and
concluded: *"Nearly half of every token this model saw was English. Modelling English
prose is not something a 1M-parameter transformer can do... so that half of the budget
buys almost nothing, while competing for the same parameters as the grammar."* The
samples back it — `getAmnestyPolicyToBmnesty`, `puzzle.persudget(puzzle)`.

**Deleting it is worse, but the experiment is confounded.** `--strip-comments` cost
**0.27 nats** at predicting code. The same notes flag why that number cannot be read
as "comments are load-bearing": stripping also **halved the corpus**, 2.93M → 1.50M
tokens. Verbatim: *"This experiment cannot separate that from the volume effect;
distinguishing them needs a run that keeps the comments but pads the corpus back."*
**That arm has never been run.** This plan is the third option neither arm tested:
keep the English, and arrive already able to read it.

**The register is far easier than it looks.** Measured 2026-08-27 over all 973 batch9
`.st` files (687 405 comment word-tokens, backticked spans folded to one symbol):

| | batch9 comments | `examples/stitch/` | TinyStories |
|---|---|---|---|
| mean words/sentence | **10.3** | — | ~8–10 |
| types covering 90% | **1 375** | 978 | ~1 500 vocab by construction |
| types covering 95% | 2 360 | 1 458 | — |
| total types | 11 345 | 1 992 | — |
| backticked-code share | **1.9%** | 7.9% | n/a |

Structurally this **is** TinyStories: same sentence length, same core vocabulary size.
The hand-written `examples/stitch/` files are the hard case — dense expert prose, 4×
the backtick density, cross-references to other repo docs — and they are ~3% of
training tokens. The 97% that actually trains the model reads like:

```
// Check if a box is lighter than another (exclusive).
// Returns the new BalancedBoat.
// A conflict exists if times overlap AND resources are the same.
```

**Why this is cheaper than general English.** The corpus needs no geography, politics,
history, sociology or literary vocabulary, and essentially no world model — only a
mathematical/programming one. Named entities are the largest parameter sink in a
general LM precisely because they do not compress: there is no rule generating
"Tallinn is in Estonia". Grammar and common-noun semantics compress well, which is why
TinyStories works at 1M. The everyday nouns that *do* appear (oysters, tides, sheep,
podcasts) need the **word**, not the referent — `// Oysters like 8.5-9.5` is a shallow
frame. And the 11k-type tail costs sequence length rather than parameters, because the
vocab is byte-level BPE: rare domain nouns decompose into subwords already present.

**Why this is the only route up the ladder.** At the frozen 2048 vocab, 9.8 MB ≈ 2.93M
tokens:

| rung | params @ vocab 2048 | tok/param | % of Chinchilla (~20) |
|---|---|---|---|
| drivel | 1.05M | 2.8 | 14% |
| quip | 3.05M | 0.96 | 4.8% |
| cliché | 9.97M | 0.29 | 1.5% |
| ballad | 29.1M | 0.10 | 0.5% |
| saga | 88.8M | 0.033 | 0.17% |

Every rung is data-starved and it worsens 100× up the ladder — which is why *"tripling
the rung bought 0.030 nats"*. Feeding saga at Chinchilla needs ~2B tokens ≈ 6.7 GB.
Stitch generation will never produce that. Pretraining is not an optimisation here; it
is the difference between cliché/ballad/saga being trainable and being shapes we can
allocate but not fill.

### Prior art

Both halves of this are trodden ground; the combination at this scale is not.

- **TinyStories** (Eldan & Li, 2023, arXiv 2305.07759 — already cited by
  [../docs/prattle-design.md](../docs/prattle-design.md)). The narrow-distribution
  half: 1M–33M models produce fluent coherent English when the *distribution* shrinks
  rather than the job. This is the evidence that TinyNerd is a shape that works at
  drivel/quip size.
- **phi-1 — "Textbooks Are All You Need"** (Gunasekar et al., 2023), plus phi-1.5 /
  phi-2. The closest prior art to this plan: a 1.3B model trained on *synthesized
  textbook-quality prose interleaved with code*, beating models ~10× larger on
  HumanEval. Its headline claim is this plan's params argument — curated data buys an
  order of magnitude of parameters.
- **Domain-adaptive pretraining** ("Don't Stop Pretraining", Gururangan et al., 2020).
  The staged half: general → continued pretraining on domain text → task, paying even
  when the domain corpus is small. At ~110M BERT scale, not 1M.

**What has no precedent I can find** is the cell this project is in: sub-100M params,
synthetic technical English as the pretrain, fine-tuned onto a language with **zero
presence in the wild**. Every code-LM result above benefits from the target language
being all over GitHub — the model arrives already knowing Python. Stitch has ~292 KB
of real code in existence. That is why the tok/param table bites as hard as it does,
and it is the reason the step-3 gate exists rather than an appeal to precedent.

Likewise unstudied at this scale: **comment↔code binding**, step 7's question.
TinyStories says 1M buys grammar; nothing says what it costs to align a prose span
with the code span beneath it.

⚠️ **These citations are from memory, offline, and unverified — check them before
leaning on any of it.** Newer work almost certainly exists.

### Two divergences to settle first

**`generative-ladder.md` describes a loss mask that does not exist.** Lines 51 and
197–205 state comments are *input-only, loss-masked* — *"comment tokens are masked out
of the loss: the model is never trained to emit them"* — and that *"the loss mask is
what makes this a capacity saving"*. `cram/src/train.rs:41`,
`cross_entropy(logits, targets, vocab)`, has nowhere to put a mask; it loops every
position and divides by `positions`. **The model is trained to generate English, with
~46% of its gradient signal.** This divergence caused a wrong conclusion in the session
that produced this plan; step 0 fixes the doc or the code, and nothing else starts
until it is settled — the answer changes what "pretraining" even means here.

**"masked NLL" is two different masks.** `cram-eval`'s gate metric is masked by the
*oracle's legal token classes*, nothing to do with comments. Do not read one for the
other.

### Overlap with `kvetch-next-measurements.md`

That plan's steps 1–2 ask whether quip's 0.258-nat advantage *"survives on code-only
text, or whether it was bought in English prose"* — the same split-metric machinery
step 2 below needs. **Build it once.** Whichever plan reaches it first owns it; the
other cites it. Do not implement a second code-only scorer.

### Measurement discipline (inherited, non-negotiable)

- Frozen `corpora/kvetch-batch9.vocab` and frozen `corpora/heldout` on **every** run.
- Always `--name`; without it a sweep overwrites itself.
- `--eval-batch 1024`, never the default 64.
- Two seeds per arm; comparisons are paired.
- Absolute NLL is not comparable across held-out sets or vocabs — only deltas within a
  condition.
- A token count is meaningless without naming its vocab.

## Acceptance Criteria

- [ ] The `generative-ladder.md` loss-mask claim and `cram`'s actual behaviour agree,
      in whichever direction is chosen, with the choice written down.
- [ ] Held-out NLL can be reported **split into code tokens and comment tokens**, and
      the splitter is verified against text whose split is known by construction.
- [ ] We know the frozen vocab's bytes/token on English prose, against the 3.34 it
      achieves on batch9 Stitch — and therefore whether pretraining through it is
      near-free or doubles the pretrain compute per byte.
- [ ] The never-run arm is run: comments kept, corpus padded back with non-Stitch
      prose. We know whether English pretraining moves **code-token** NLL at all.
- [ ] If it does: a TinyNerd corpus exists as a first-class frozen artifact —
      manifested, cached, deduped, leak-checked against `corpora/heldout` — with its
      generator and seed pinned and the corpus itself gitignored.
- [ ] A pretrain → fine-tune checkpoint beats a from-scratch one at equal total
      compute on **code-token** held-out NLL, or the plan records that it does not and
      why.

## Steps

Every step follows RED-GREEN-MUTATE-KILL MUTANTS-REFACTOR. No production code without
a failing test. Steps 1–3 are cheap and gate the expensive ones.

### Step 0: Reconcile the loss-mask divergence

**Acceptance criteria**: `generative-ladder.md` and `cram/src/train.rs` no longer
contradict each other. Either `cross_entropy` grows a mask parameter and the ladder's
written decision is implemented, or the doc is corrected to say comments are trained
on like any other token — with the reasoning recorded either way. A reader can no
longer conclude the opposite of the truth from the doc, which is what happened here.
**Present the direction to the human before writing code** — implementing the mask is
a modelling change with its own consequences, not a doc fix.
**RED**: If implementing — a test that a masked position contributes zero loss and
zero gradient, and that the mean divides by unmasked count, not `positions`.
**GREEN**: Minimum mask threading through `cross_entropy`.
**MUTATE / KILL MUTANTS / REFACTOR**: standard.
**Done when**: doc and code agree; direction approved by the human.

### Step 1: Measure the frozen vocab on English

**Acceptance criteria**: A reported bytes/token for English prose tokenized through
`corpora/kvetch-batch9.vocab`, beside batch9's 3.34 on Stitch, over at least two
English sources (repo markdown; one external technical corpus). Interpretation stated
up front so it cannot be rationalised after: **~2.5–3.0** → pretraining through the
frozen vocab is near-free and wire law is untouched; **~1.5** → English shreds,
pretrain compute roughly doubles per byte, and a vocab decision must precede
everything else.
**RED**: A test that the tokenizer round-trips English text byte-exactly (byte-level
BPE claims this; it should be asserted, not assumed) and that bytes/token is computed
over the right denominator.
**GREEN**: A small reporting path — likely a `cargo xtask cram` flag or a test-only
helper, not a new binary.
**Done when**: the number exists and the branch it selects is recorded here.

### Step 2: Split the gate metric into code tokens and comment tokens

**Acceptance criteria**: `cram-eval` reports held-out NLL separately over code
positions and comment positions. Verified against a hand-built program whose split is
known by construction, not against another implementation. Without this the whole plan
is untestable: pretraining on English will improve comment prediction a lot and code
prediction maybe a little, and a single blended number improves either way — a control
that cannot discriminate.
**RED**: A held-out fixture with a known code/comment token split; assert the two
reported means recombine to the blended mean at the right weights.
**GREEN**: Reuse `strip_comments` (`xtask-cram/src/corpus.rs:204`) to derive positions
rather than writing a second comment lexer — it already handles `//` inside string
literals.
**MUTATE / KILL MUTANTS**: a pass-through is not covered by its delegate's tests.
**Done when**: existing checkpoints (`drivel-b9b10-30k`, `quip-D-60k-s1`) each report a
code/comment split. Coordinate with `kvetch-next-measurements.md` steps 1–2.

### Step 3: Run the never-run arm, with prose already on disk

**Acceptance criteria**: Two paired runs, two seeds each, drivel, 30k steps, frozen
vocab and held-out: (A) corpus as today; (B) corpus plus the **5.27 MB of repo
markdown** (`plans/`, `docs/`, `posts/`, `notes/`, excluding `corpora/` and
`.claude/`), which is ~+70% tokens in exactly the register the comments imitate. Report
the step-2 split. The question this answers: does non-Stitch English move **code-token**
NLL, or only comment-token NLL?
**Leak check is mandatory and is the trap**: held-out exclusion is keyed on
*comment-stripped* text, so it cannot catch a held-out program whose **comments**
paraphrase a repo doc. Run line-level Jaccard over all (repo-doc × held-out) pairs, as
batch10 did over its 712 × 116, and report the max before quoting any number.
**RED**: A test that the corpus loader admits a non-`.st` prose source without
corrupting the `Layout` separator handling — *train on the programs, never the corpus
file*.
**GREEN**: Minimum loader change; likely a `--prose-root` sibling to `--real-root`.
**Done when**: four runs, split metric reported, leak max reported. **This is the
gate.** If code-token NLL does not move, stop here and record the negative result —
steps 4–7 are days of generation compute.

### Step 4: Specify the TinyNerd corpus

**Read phi-1's data-synthesis methodology first.** "Textbooks Are All You Need"
(Gunasekar et al., 2023) is the nearest published attempt at exactly this artifact —
synthesized textbook-quality prose built to train a code model — and it is the one
piece of prior art likely to *change what this step writes* rather than merely
justify it. Specifically: how they enforced topic/vocabulary diversity without
hand-listing domains (the failure mode this step's exclusion list courts), how they
interleaved prose with code rather than keeping them in separate files, what their
filtering/classifier stage rejected, and what diversity collapse looked like when
generating at volume from one model. batch9 already hit the volume-diversity problem
from the other side — 45 degenerate files, repetition pathologies — so this is a known
sharp edge here, not a hypothetical. **Do not write the recipe before reading it**;
budget a literature pass as part of this step, and record what was adopted and what
was deliberately not.

**Acceptance criteria**: A recipe in the shape of `batch9.toml`
(`cram-gen/src/recipe.rs`) that pins: a controlled core vocabulary target (~1 500
types at 90% coverage, matching the measured batch9 comment profile), a domain list,
target sentence length ~10 words, an explicit **exclusion list** (geography, politics,
history, named people and works, literary vocabulary), and the prose form — short
declarative statements about functions, maps, lists, ranges, invariants. Plus the
validators that reject non-conforming output. **Plan-only step; no generation.**
**Done when**: the recipe is reviewed and the exclusion list is agreed.

### Step 5: Generate a pilot batch and measure yield

**Acceptance criteria**: A pilot of ~50 candidates through the existing `cram-gen`
pipeline, reporting tok/s and rejection rate against batch9's **36.6 tok/s with 45%
parse deaths**. The economic claim to verify: TinyNerd has **no parse gate**, so
rejections should be validator-only and yield should be substantially better — if it
is not, the cost model for step 6 is wrong and the sizing must be redone.
**Done when**: yield numbers exist and a full-batch cost estimate follows from them.
Estimate the corpus size needed to move drivel *and* the size needed to feed cliché;
they differ by orders of magnitude and only one may be affordable.

### Step 6: Freeze the corpus and run pretrain → fine-tune

**Acceptance criteria**: TinyNerd frozen with manifest, cache and dedup like
`corpora/batch9`; leak-checked against `corpora/heldout` by the step-3 method.
Then paired runs at **equal total compute**: (A) from-scratch on Stitch; (B) pretrain
on TinyNerd, then fine-tune on Stitch. Two seeds. Report the step-2 split.
**Open question to settle before running, not after**: at drivel, a fine-tune corpus
much larger than the pretrain will discard the English fast, so mixing may beat
two-phase at the bottom of the ladder while two-phase wins higher up. Decide which is
arm B, or run both and say so.
**Done when**: the comparison exists at equal compute, with the split metric.

### Step 7: Decide what it bought, and for which rung

**Acceptance criteria**: A written verdict covering: whether code-token NLL improved;
whether the improvement grows with rung size (the prediction is that the English is
cheap at drivel and the **comment↔code binding** is what wants cliché); and whether
TinyNerd changes the tok/param table enough to make cliché trainable. Sample output
printed beside every metric — a bare number has blamed the model before when the
corpus was at fault.
**Done when**: the verdict is in `notes/`, and this plan is archived to `plans/legacy/`
per CLAUDE.md.

## Risks

- **The gate metric measures the wrong half.** ~46% of held-out tokens are comments;
  pretraining will move them most. Step 2 exists solely to stop this. Do not quote a
  blended number anywhere in this plan's results.
- **Leakage through prose.** Repo docs and held-out comments share an author and a
  domain. The existing exclusion is comment-*stripped*-keyed and structurally cannot
  see it. Step 3's Jaccard sweep is not optional.
- **Sunk generation cost.** Steps 4–6 are days of compute. Step 3 gates them on ~4
  drivel runs using prose already on disk. Do not reorder.
- **Kernel image budget.** Nothing here promotes a checkpoint. The itest kernel image
  embeds every program and already hit `OutOfFrames` at 4.5 MB; a larger rung reaching
  the target is a separate decision with its own plan.
- **The corpus is not purely technical.** batch9's briefs bring oysters, tides and
  podcasts. The exclusion list in step 4 must exclude *categories that need a world
  model*, not *every concrete noun* — over-narrowing produces prose unlike the target.

## Pre-PR Quality Gate

1. `cargo xtask test && cargo xtask itest && cargo xtask itest --scramble`
2. `cargo xtask clippy`
3. Mutation testing on new host logic (`cram-eval` splitter, corpus loader)
4. `cargo xtask links` and `cargo xtask plan-status`
