# prattle: a code captioner in a child's vocabulary

**Status:** 📐 **DESIGN — deferred, deliberately.** Nothing here starts until
the [generative ladder](generative-ladder.md) ships a working *comment-free*
model. prattle is **off-ladder**: a sibling to drivel/quip/cliché/ballad, never
a rung. It does a different job (code → prose, i.e. captioning), on different
data, with a different evaluation, and it never participates in speculative
decoding.

The idea: a small model that reads a Stitch function and says what it does in
the ~1500 simple English words a small child knows — the
[TinyStories](https://arxiv.org/abs/2305.07759) vocabulary, moved from stories
to code captions. Cute on purpose, and the cuteness is load-bearing (see
"register as provenance").

Related: [generative-ladder.md](generative-ladder.md) (the comments decision
this branches off — input-only, loss-masked, uniform across rungs),
[llm-design.md](llm-design.md) (corpus tiers, verification stack, provenance),
[stim-design.md](stim-design.md) (where captions surface),
[../plans/drivel.md](../plans/drivel.md) (the training rig prattle reuses).

---

## Why a separate model, not a mode of the code model

Every difficulty in the comments discussion came from one model doing two
jobs. A sibling dissolves all of them at once:

| Problem when comments live in the ladder | With a separate captioner |
|---|---|
| Vocabulary must cover English → bloat, byte-shredding | Ladder vocab stays Stitch-only and frozen |
| Prose competes with code for 10–30M params | Separate parameters, separate budget |
| Per-rung policy differences collapse spec-decode acceptance | prattle never drafts or verifies; no interaction |
| Comment generation is unverifiable by the code stack | Gets its *own* check (below) |

It is also a genuinely different **task**. The ladder predicts the next token
of a code stream; prattle maps a whole function to a short description —
summarisation, not continuation. The image-captioning analogy is exact, and so
is the honesty it implies: prattle is a *captioner*, not a documenter. It
produces something plausible and often useful, sometimes wrong, and obviously
machine-made.

## Register as provenance

The generated register is unmistakable:

```stitch
/// takes a list and gives back how many things are in it
count(items) = ...
```

versus a human's:

```stitch
// The operator-pending accumulator — the whole partial command: the pending
// operator, a count prefix, and a text-object prefix. A field on `Editor`
// (not a mode) — operator-pending stays *within* Normal mode.
```

Nobody confuses the two. The hazard with model-authored prose is that it is the
artifact readers trust most and verify least; a picture-book register
**announces itself**, in-band, at a glance, with no metadata and no way to
forge it by accident. That is a real security-ish property falling out of an
aesthetic choice, and it is why the cuteness is not decoration.

Substrate labelling still applies on top: a caption accepted into a buffer is
provenance-marked model-authored like any other model-authored bytes.

## Executable back-translation: the check comments were said to lack

The standing argument against generating comments is that the verification
stack (parse → typecheck → tests → authority) cannot check whether prose is
*true*. That remains so for **accuracy** — but **sufficiency** is checkable,
and we own every piece needed:

> caption function `F` → have a strong model regenerate `F′` from the caption
> alone → run `F′` against `F`'s tests.

If `F′` behaves like `F`, the caption carried enough information to determine
the function. This is back-translation (as in machine translation) with
*execution* as the comparison, and it is tractable here precisely because we
own the interpreter, the test corpus, and the language.

**Honest limits.** (1) It checks sufficiency, not accuracy or style: a caption
can be sufficient and still read badly. (2) The regeneration step needs a model
that can write code from a description — exactly the capability the small rungs
lack — so this runs **offline with a frontier model, as part of prattle's
evaluation**, never on-device at inference. Evaluation does not have to be
cheap. (3) It scores best on functions whose behaviour the tests actually pin;
a thin test suite makes a weak oracle.

Metrics, then: back-translation pass rate (headline), vocabulary conformance
(set membership against the whitelist — free), length distribution, and human
spot-checks on a fixed sample.

## Training data arrives free

Every Tier-2 corpus generation call can emit a simple-register caption
*alongside* the program it produces — same call, same context, no extra
inference. So prattle's `(function, caption)` corpus accumulates as a
byproduct of the corpus pipeline being built anyway, and inherits its
validators: a program that fails parse/typecheck/tests takes its caption with
it, and a caption that fails the vocabulary whitelist is rejected on the spot.

Two corpus rules carry over from the comments decision and matter more here:

- **Signature-adjacent captions only; never rationale.** "Operator-pending
  stays within Normal mode, so the cursor still shows" states a design
  consequence *not present in the code*. Training on rationale-shaped prose
  teaches confident invention of design intent — the worst possible failure in
  the most authoritative-sounding register in the file. prattle describes
  *what a function does*, never *why it was built that way*.
- **The valuable comments are the unlearnable ones.** prattle is aimed
  squarely at the low-value, high-frequency band — restating behaviour — and
  that is the correct target, not a consolation.

## Why prattle should not feed the completer

An appealing idea — let captions compress distant code into a small context
window — considered and **rejected 2026-07-25**. Any error in a caption is
compounded by the model that writes code from it, and the compounding is worse
than additive:

- **The context window erases the distinction between observation and
  inference.** Code in context is ground truth; a caption is a guess. To the
  completer both are just tokens, equally authoritative, so a wrong caption is
  not noise — it is confidently wrong evidence with no signal to discount it.
  (Partial mitigation, free from the register choice: because captions are in
  unmistakable simple register, a model *could* learn that such text is
  inferred and weight it down — the register serving as epistemic status for
  the model as well as provenance for the human. Whether a small model learns
  that is doubtful.)
- **There is a path back into the corpus.** A caption accepted into a buffer,
  in a file that later becomes training data, trains the model on its own
  errors — model collapse in miniature, through a door the Tier-3
  self-distillation warning does not cover.

What would survive is captions as a **verified build artifact** rather than
live inference: generated offline, each gated on back-translation, cached, so
the completer only ever sees captions that provably regenerate their function.
That bounds the error where a frontier model is affordable.

**But the compression case is weak on its own terms.** For fitting distant code
into a small context there is a strictly better compression: **structural
elision — keep signatures and types, drop bodies.** That is lossy but *true*,
so it never introduces a falsehood and never compounds; semantic compression is
lossy *and* inferred, so it does. Captions only beat signatures where
signatures are uninformative (untyped parameters, vague names), and Stitch has
types plus owned naming conventions.

So the boundary stands: **prattle is human-facing, and model-generated text
never enters model input.** Easy to state, easy to enforce, and immune to this
whole class of problems by construction.

## Staging

| Stage | What | Gate |
|---|---|---|
| A | Ladder ships comment-free: stripped from corpus, `<comment>` sentinel for out-of-register input | A working model exists at all |
| B | Measure whether comments-in-context help FIM (masked NLL, existing harness) | May honestly say "no" — which settles input-only empirically rather than by argument |
| C | Train prattle on the free `(function, caption)` pairs | Back-translation pass rate beats a trivial baseline (e.g. captioning by function name alone) |
| D | stim surfaces captions as a distinct affordance | Never auto-inserted; explicitly machine-marked; provenance-labelled if accepted |

Stage B is worth running even if prattle never happens: it is cheap, reuses
drivel's harness, and turns "comments probably do not help at this scale" from
an assumption into a measurement.

## Why this is worth doing at all

It is a real extension of the TinyStories result rather than a costume: the
same finding — *restricted vocabulary makes small models coherent* — moved
from stories to code captions, with an execution-based check the original
never had. That combination (owned language, owned interpreter, owned corpus,
owned editor to display it in) is not available to anyone working on
mainstream languages, which makes it the most publishable small artifact in
the arc.

The name fits the family: **prattle** — simple, idle chatter. Off-ladder, but
plainly related.

## Open questions

- Caption granularity: per-declaration only, or also per-block? (Per-block
  needs the model to pick *what deserves a comment*, a harder and separate
  judgement.)
- Does the captioner share the ladder's vocabulary (Stitch tokens plus the
  simple-word set) or carry its own? Sharing eases tooling; separating keeps
  the ladder's freeze wholly untouched. Leaning separate, since nothing
  requires them to compose.
- Is a caption conditioned on the function alone, or on its tests too? Tests
  pin behaviour far better than a body does, and we have them.
- Whether stage B's answer changes anything: if comments-in-context measurably
  help FIM, does the sentinel become a *summary* token produced by prattle —
  the captioner feeding the completer? **Answered in outline (2026-07-25): if
  so, caption the *code*, never simplify the user's prose.** Text
  simplification splits into two tasks that get conflated. *Surface*
  simplification (shorter sentences, commoner words) is genuinely tractable for
  small models — the Wikipedia→Simple-Wikipedia line of work. *Conceptual
  re-explanation* is not, and the reason is informational rather than a matter
  of capacity: a comment like "the soft form of the future clipboard-service
  entry" is mostly a **pointer into unstated design context**, so there is
  nothing in the input to simplify and the output collapses to fluent vacuity
  ("it is made"). Same shape as the rationale-comment finding — the hard cases
  are hard because the information is absent, and scale does not supply it.
  Captioning the code sidesteps the simplification problem, but **the
  compression argument for it does not survive scrutiny — see "Why prattle
  should not feed the completer" below.**
  Falsifiable and cheap if it ever matters: gold-simplify `stim.st`'s comments
  with a frontier model, train a small simplifier, and score in-register
  (whitelist membership, free) *separately* from still-true (spot-check).
  Prediction: high in-register, low still-true, failures concentrated in the
  context-pointer comments.
