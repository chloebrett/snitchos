# The grammar mask: turning a token model into a class distribution

**Status:** 📐 **DESIGN — not started.** The missing piece between
[`cram-eval`](../plans/drivel.md)'s harness and its purpose: today `--eval` can
print a checkpoint's parse rate but refuses to print its masked-NLL row, because
no `Predictor` exists for a trained rung.

The same table serves two consumers, which is why it gets its own doc rather
than living inside the eval:

- **Scoring** — `p(class | prefix)` so a model can be compared to babble on the
  gate metric.
- **Constrained decoding** — the set of vocab tokens admissible at a cursor, so
  a masked model physically cannot emit illegal Stitch
  ([generative-ladder](generative-ladder.md), [llm-design](llm-design.md)).

Get the first right and the second is nearly free. Get it wrong and the ladder's
headline number is a plausible figure that measures nothing.

---

## The problem: two event spaces that do not line up

The oracle speaks in **token classes** at **token boundaries**:
`valid_next(src, pos) -> TokenSet`, 59 classes, one decision per Stitch token.

A trained rung speaks in **BPE tokens** at **BPE boundaries**: a distribution
over ~571–1024 vocab entries, one decision per subword.

Masked NLL needs `p(class)`. The model offers `p(vocab token)`. These are not
the same event, and the conversion is where a wrong number would hide.

### Four concrete misalignments

**(A) One class, many tokens.** A lexeme may take several vocab entries. If
`contract` encodes as `" cont"` + `"ract"`, the class `Contract` corresponds to a
*sequence*, and its probability is a product, not a lookup.

**(B) One token, many classes.** With GPT-2 pre-tokenization a chunk contains no
internal whitespace, so `"()"` is one chunk and BPE may merge it into a single
entry. Emitting it discharges `LParen` *and* `RParen` — two grammar decisions in
one model step. The decision counter itself becomes ill-defined.

**(C) Payload classes have unbounded spellings.** `Ident` is any of thousands of
names. `p(Ident)` is a sum over every token sequence forming an identifier, not
over one lexeme.

**(D) Shared prefixes.** `"<"` begins both `Lt` and `Le` (`<=`). A single vocab
token's mass belongs to two classes and cannot be split without looking ahead.

(D) is the one that kills the naive approach and (B) is the one that is
conceptually worst.

## Why the obvious three answers fail

**First-token aggregation** — `p(class) ∝ Σ p(t)` over tokens that could begin
it — is the approach everyone reaches for. (D) breaks it: the sets overlap, the
same mass is counted under `Lt` and `Le`, and renormalizing a double-counted
total produces something that sums to 1 and is a distribution over nothing.
Silent, plausible, wrong — the exact failure mode this doc exists to avoid.

**Exact marginalization** over all token sequences spelling a class is correct
and intractable: unbounded for `Ident`, and many forward passes per decision
against a scorer that already costs ~13 ms per decision.

**Scoring the model on its own terms** — per-BPE-token or per-byte NLL of the
canonical encoding — is exact, standard, cheap, and *exactly what training
optimized*. It fails the one requirement that matters here: babble has no
token-level distribution at all, so this abandons the single scoring path the
`Predictor` trait exists to guarantee. It stays valuable as a **model-vs-model**
number and should be reported; it cannot be the gate.

### The tempting decomposition, and why the plan already rejected it

A decision is really two: *which class*, then *which spelling within it*.
`NLL_total = NLL_class + NLL_spelling|class`. Both rungs technically have both —
babble draws identifiers uniformly from a 30-word list.

That list is why it cannot be used. babble assigns probability **zero** to any
identifier outside its wordlist, so the first real identifier in held-out Stitch
scores infinite NLL and the metric is decided by one token. Class-level is not a
simplification; it is the only level at which rung 0 can play at all. This is
what the plan meant by "distributions over that set", and it is right.

## The fact that makes this tractable

Increment 1 measured something that changes the calculus:

> **babble's lexicon saturates at 571 tokens.** 721,876 tokens for 721,876
> whitespace lexemes — *exactly one token each*.

At the probe's vocab size the BPE vocab is, in practice, a **lexeme vocab**.
Case (A) mostly evaporates: a keyword or operator is one entry, so `p(class)` is
one lookup rather than a product.

**This is the load-bearing assumption of the whole design, it is measured only
for babble-generated text, and increment 6 scores real Stitch.** Real
identifiers (`operatorPending`, `charwise`) are absent from a babble-derived
vocab and *will* be multi-token. So the assumption must be re-measured against
the held-out corpus before any number is trusted — see Gate 0.

## Recommended design: complete-lexeme token sets, with the residue measured

**One table, built once per (vocab, grammar):** for each token class, the set of
vocab entries that constitute a *complete* lexeme of that class.

```
ClassMask {
    complete: [TokenSet; CLASS_COUNT],   // vocab entries that finish a lexeme
    partial:  [TokenSet; CLASS_COUNT],   // entries that only start one
}
```

At a decision with legal set `L`, from **one** forward pass:

1. `p(class) ∝ Σ p(t)` for `t ∈ complete[class] ∩ (tokens whose class is in L)`.
2. Renormalize over `L`.
3. Floor every legal class at a small ε, because a zero is an infinite NLL and
   one such position swamps the corpus mean — the same rule `Predictor` already
   documents.

The overlap problem (D) disappears from `complete` by construction: a vocab
entry spelling `<` completes `Lt` and *only* `Lt`; `<=` completes `Le`. Shared
prefixes live in `partial`, which scoring ignores and **decoding needs** — the
two consumers want different halves of the same table, which is the argument for
building it once.

### What this deliberately does not model

Mass sitting in `partial` (multi-token lexemes) and mass in entries spanning two
classes (B) are **not** scored. That is an approximation, and the discipline is
to *measure* it rather than argue about it:

> **Report `mask_coverage`: the fraction of model probability mass, per
> decision, that lands on a complete-lexeme entry for some legal class.**

At coverage ~99% the approximation is noise. At 80% the masked-NLL row is not
trustworthy and the note is wrong. Printing it beside the number is the same
rule as printing samples beside a parse rate — the eval's job is to make its own
weakness visible.

## Gates, in order

**Gate 0 — measure the assumption before building anything.** Encode the
held-out corpus with the shipped vocab and report: lexemes per vocab entry,
what fraction of Stitch lexemes are exactly one entry, split by class, and how
many entries span a class boundary. **If one-token-per-lexeme does not hold on
*real* Stitch, this design changes** — the fallback is scoring only
`has_one_spelling` classes exactly and reporting payload classes separately,
accepting reduced coverage in exchange for exactness. Cheap, read-only, no model
required, and it decides the shape.

**Gate 1 — the mask is sound.** Every class in `complete` re-lexes to that
class; no entry completes two classes; `complete ∩ partial = ∅`. Property tests
over the whole vocab, not examples.

**Gate 2 — the distribution is well-formed.** Sums to 1; support ⊆ legal; every
legal class has positive mass. These already exist for babble and uniform and
should be extended to the model impl rather than rewritten — the `Predictor`
trait exists so that a new rung inherits its tests.

**Gate 3 — calibration.** The clairvoyant control (a predictor that knows the
answer scores ~0) already guards the scorer. The mask needs its own: a model
whose logits are uniform must produce the same row as `Uniform`. If it does not,
the mask is biasing the comparison rather than measuring it.

**Gate 4 — only then, the drivel row.** With `mask_coverage` printed beside it.

## Open question worth deciding early

**Does the decision *count* stay the oracle's?** Case (B) means one model step
can discharge two grammar decisions, so "mean NLL per decision" has a different
denominator for the model than for babble. Options: keep the oracle's decision
count as canonical and attribute a spanning entry's mass to the first class
(simple, slightly penalizes the model); or exclude decisions reachable only via
a spanning entry (unbiased, loses coverage). **Recommend the first, with the
affected fraction reported** — the bias is toward the model losing, which is the
safe direction for a claim of the form "the trained rung wins".

## Relationship to the rest of the ladder

- **Constrained decoding** takes `complete ∪ partial` at a cursor, on-target.
  Same table, no floats needed for the mask itself.
- **Speculative decoding**'s soundness requires draft and target to be masked
  identically ([generative-ladder](generative-ladder.md)); one shared table is
  how that stops being a thing to remember.
- **Forced tokens** — the 800-of-10,950 singleton positions the eval already
  counts — skip the forward pass entirely. The eval's `forced` column is the
  first measurement of that saving.
- **The vocab freeze** is what lets this table be built once per ladder rather
  than per rung.
