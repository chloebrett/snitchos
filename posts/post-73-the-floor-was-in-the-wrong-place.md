# Post 73 — the floor was in the wrong place

*Lab notebook. I built the thing that measures the models, and the first number
it produced was that the baseline they were supposed to beat had never been
measured either — and loses to a model with no parameters, no tables, and no
opinions.*

---

## Why build the ruler before the thing it measures

[drivel](../plans/drivel.md) has seven increments. The plan's order puts the
real corpus at increment 2 and the eval harness at 3, and I did them the other
way round, because the corpus is the long pole (Stitch has ~2K lines of itself
in existence) and the harness is an afternoon. Everything downstream of a ruler
is guesswork until the ruler exists.

The state going in: [drivel](post-63-drivel-speaks-stitch.md) — a 918,656
parameter transformer — had been trained on [babble](post-64-a-model-with-no-weights.md)
output and scored 91% unconstrained parse. That answered *can a model this
small learn the grammar*. It said nothing about Stitch as anyone actually
writes it, because babble's output is legal and meaningless by construction.

The gate metric for the real question is **held-out masked NLL**: at each
position of a program a human wrote, the continuation oracle names the token
classes that are legal there; every rung is a distribution over exactly that
set; score the negative log-likelihood of the class the human chose. Exact, no
sampling, and stable on the few thousand held-out tokens that exist.

## One scoring path, or the rows drift apart

The harness is `cram-eval`, and its whole shape is one trait:

```rust
pub trait Predictor {
    fn name(&self) -> &'static str;
    fn weights(&self, at: Context<'_>, legal: TokenSet) -> Vec<(TokenClass, f64)>;
}
```

babble implements it from its bias tables; a trained rung will implement it from
masked logits. The point is not elegance — it is that babble's floor row and
drivel's row are produced by *the same code*, so they cannot become
incomparable through drift. "babble is rung 0 of the same ladder" stops being a
claim in a design doc and becomes a compiled one.

The legal set is passed *in* rather than computed per rung, which costs one
parse per class to derive and guarantees both rungs are scored against the
identical set.

### Why the comparison has to happen at the class level

There is an obvious better metric sitting right there. A decision is really two:
*which class*, then *which spelling within it*. Real Stitch writes both, and
`NLL_total = NLL_class + NLL_spelling|class`.

babble is why we cannot have it. babble draws identifiers uniformly from a
30-word list, so it assigns probability **zero** to any identifier outside that
list — and the first real identifier in held-out Stitch scores infinite NLL. The
whole metric would be decided by one token. Class-level is not a simplification;
it is the only level at which rung 0 can play at all.

### The API babble needed, and the reason it returns integers

babble had `pick` — draw a class — and nothing that could say `p(class | prefix)`.
Scoring needs a density, not a draw, so it grew one:

```rust
pub fn distribution(tables: &Tables, legal: TokenSet, emitted: u32, depth: u32)
    -> Vec<(TokenClass, u32)>
```

Integer weights, not probabilities, and not for purity. **babble runs on the VF2
as the model behind the kvetch endpoint, and `sstatus.FS` is never set — a
single float in userspace panics the kernel.** Normalizing is the host's job.
`p(class) = weight / Σ weights`, documented, and the division happens where
floats are legal.

The risk with two entry points into one sampler is that they describe different
samplers. So there is a test that draws 5,000 times from `pick` with a fixed
seed sequence and checks the histogram against what `distribution` announced —
on **support** first (a class one can draw and the other omits is the drift that
matters most, and a probability comparison alone would report it as a small
numeric gap) and then on probabilities. Statistical but not flaky: the seeds are
fixed, so the histogram is a constant.

## The finding: uniform-over-legal beats babble by 2.65 nats

Scored over every real `.st` file in the repo — 9 programs, 74.5 KB, 10,950
decisions, 800 of them forced:

| rung | nll | free-nll | perplexity | free-nll over first 50 tokens |
|---|---:|---:|---:|---:|
| babble (tuned tables) | 5.001 | 5.395 | 220.4 | 2.619 |
| uniform-over-legal | 2.541 | 2.742 | **15.5** | 2.062 |

The plan said babble was the floor. babble loses to `1/|legal|` — a predictor
with no tables, no tuning and no notion of anything — by a factor of 14 in
perplexity.

**The consequence is not cosmetic.** drivel's win condition was "beat babble".
That is 5.395. Had drivel scored 4.0, the plan as written would have recorded a
win, while losing to the most trivial control available. The bar moved to
**2.742**, and the plan now says the floor is the best zero-weight baseline
rather than naming babble specifically.

### The diagnosis is regime, not quality

A bad number with no explanation is barely better than no number. babble's
tables are damped by how far along and how deep the walk is: closers and `Eof`
are multiplied by a pressure term, everything else is divided by its square or
cube. That pressure saturates at `PRESSURE_CAP` (16) on a long file, throwing
nearly all the mass onto closers and `Eof`.

That is a state babble **never generates in**. Its own walks wind down at 24
tokens and its programs average ~27. Scoring it at token 8,000 of `stim.st`
asks it a question it was never built to answer.

So the report carries a `free<50tok` column, and it is the whole story:

- babble: **2.619** over the first 50 tokens, **5.395** overall.
- uniform: **2.062** / **2.742** — flat, as a rung with no notion of position
  must be.

babble is competitive early and collapses late. It is a fine *generator* of
short programs and a bad *model* of long files, and those are different jobs.
Without that column the finding reads as "the baseline is bad" instead of "the
baseline is being asked the wrong question".

### The forced-decision column, which turned out to matter

800 of the 10,950 decisions have exactly one legal class. Every rung scores
those at exactly 0 — there is no prediction to make — so they only dilute a
comparison. The report prints `nll` (everything) and `free-nll` (decisions with
an actual choice) side by side, and the gate uses the second.

That column has a second life: forced positions are exactly the ones a decoder
can skip the forward pass on entirely. It is the first measurement of a saving
that [speculative decoding](../docs/speculative-decoding-design.md) has been
assuming.

## The control I had to redesign, twice

The plan specified a calibration test: *a uniform control (babble with flat
tables) scores strictly worse than babble with its tuned tables* — proving the
harness can detect the signal it exists to measure, before a real model is on
the line.

Both halves of that sentence turned out to be wrong.

**"babble with flat tables" is not uniform.** Setting every base weight equal
still runs them through the pressure machinery — closers ×p, obligations ÷p³ —
so a flat-table babble is a *differently* biased sampler, not an unbiased one.
The control has to be a separate predictor that returns `1/|legal|` and means it.

**"tuned beats uniform" is false**, which is the finding above. A calibration
test that asserts it would have failed, and the temptation at that point is to
weaken the test rather than believe it.

So the calibration is a **clairvoyant** predictor instead: one that reads the
program being scored, puts nearly all its weight on what the human actually
wrote, and must score ~0. It checks the same property — can this harness see
signal — without assuming the answer to the question the harness exists to ask.
If a cheater does not score zero, the scorer is broken and every comparison it
produces is noise.

## What the harness found on its first run

Two bugs, which is mostly the point of building one.

### `[:]` was unreachable under a grammar mask

The very first scoring run reported: *the oracle rejected 1 token a human
actually wrote*. `plans/lang/samples.st:79`:

```
|> fold([:], (counts, w) ->
```

`[:]` is the empty-map literal. The parser accepts it. `valid_next` does not
admit `Colon` after `[`.

The cause is a two-token lookahead in a one-token-at-a-time world.
`parse_collection` recognised the empty map with

```rust
if peek() == Colon && peek_at(1) == RBracket { … }
```

which is correct on a whole program. But the oracle answers "may this class
follow?" by appending **one** candidate token plus `Eof` and reading *where*
the parse fails — error at the appended token means dead, error beyond it means
viable. Probing `[` with `:` makes `peek_at(1)` be `Eof`, so the branch is
skipped, the parser falls through to parsing an expression starting at `:`, and
the error lands **on** the colon. Dead prefix.

The consequence was not a diagnostic nicety: **`[:]` could not be reached under
a grammar mask at all.** babble can never generate one. No masked model can ever
emit one, however much training data contains it. A legal construct had been
quietly deleted from the reachable language.

The fix commits on the single decisive token — a map entry cannot start with its
own separator, so a leading `:` can only be the empty map — and *demands* the
`]`:

```rust
if matches!(self.peek(), TokenKind::Colon) {
    self.bump();
    self.expect(&TokenKind::RBracket, "']' to close the empty map `[:]`")?;
    return Ok(self.spanned(start, ExprKind::Map(Vec::new())));
}
```

The failure moves one token later, to where the oracle correctly reads
"consumed it and wanted more". Accepted language unchanged; only the error
position moves. 672 unit tests and every integration suite pass, so nothing
depended on the old placement.

**The general lesson is worth more than the bug.** A parser branch that needs
*k > 1* tokens of lookahead to commit has a prefix its own oracle calls dead,
and every such branch is a hole in the decode mask. `peek_at(` is now a
grep-able smell.

This is [post 64](post-64-a-model-with-no-weights.md)'s direct sequel. That post
sold trial-by-append and the property that the oracle cannot drift from the
grammar. This is the discovery that the relationship runs both ways: the oracle
is also a **property test on the parser's incrementality**, and scoring real
human Stitch is what makes it fire. It is the second time the one-token contract
has caught a latent assumption — the first was maximal munch and the
load-bearing space in `> =`.

### The printer bug, and the over-correction that was caught just as fast

While regenerating the corpus, babble's round-trip fuzzer failed at seed 27. The
printer's contract is `parse(print(ast)) == ast`, and:

```
parsed:   (expect (@? / depth / ..=)) ?. port
printed:  expect (@? / depth / (..=)?.port |> …)
```

`bp_of` — how tightly an expression binds *as a whole* — scored `Expect` as
`ATOM_BP` via the fallthrough, meaning "already delimited, needs no help". True
of a list or a block. False of `expect`, which swallows an arbitrary operand at
the loosest binding power and closes with nothing. In a postfix position it
needs parentheses, or on reprint it eats the field access and everything to the
right of it.

I moved `Expect` to `LOOSEST_BP`, and moved `Handle` and `Without` with it,
reasoning that they also take loose operands.

The fuzzer failed at seed 2 immediately. `handle` and `without` *require* a `{`
body in the parser, so they always end in `}` and a following postfix binds to
them unaided. Parenthesising them is not merely noisy — the added leading `(`
gets read as a **call by the previous statement**. Over-parenthesising is not
the safe direction.

Narrowed to `Expect` alone. Both cases now have hand-written tests, so they no
longer depend on a fuzzer seed staying put. The thing I keep relearning: the
instrument that finds the bug finds the bad fix at the same speed, and that is
most of its value.

## A ritual turned out to be a missing cache key

Standing instruction in this repo, given to me at the start of the session:
*after any printer change, delete the corpus manifest, because the cache digest
cannot see a fix touching one program in 246,000.*

That is a correct workaround for a real defect, and workarounds that live in a
human's head get skipped. The manifest fingerprinted **three** programs. Two
different failures had already happened:

- Adding the `test` keyword made a 59th token class, which shifts every draw in
  every babble walk — so *every* program changed. A 3-seed probe catches that.
- Fixing the `expect` printer changed how ~1% of programs render. A 3-seed probe
  misses that ~97% of the time.

Two changes, one for each failure mode:

- **`Manifest::grammar_digest`** — FNV over every token class and its spelling.
  It does not look at output at all, so it is deterministic rather than sampled:
  a keyword added, removed or respelled invalidates every cached corpus, with no
  luck involved.
- **`PROBE_COUNT` 3 → 256.** Misses a 1%-of-programs change ~8% of the time
  instead of ~97%, and catches anything touching 5% essentially always. Costs
  ~0.5 s on a cache *hit*, against ~8 minutes to regenerate.

Plus `FORMAT_VERSION` 2 → 3, so every manifest written before the grammar digest
existed is invalid by construction rather than by luck.

The residue is stated in the code rather than papered over: a printer change
touching well under 1% of programs is still invisible to a sample, and nothing
short of regenerating can see it.

## The keyword that broke a constant

Working in a repo with other agents editing it live, `stitch` was mid-edit for
much of the day — four separate waits for it to compile again. One of those
edits landed the `test` keyword, and `babble` did this:

```rust
const CLASS_COUNT: usize = 58;
```

A 59th class made `base[TokenClass::Eof as usize] = 2` a compile-time panic. Not
a subtle failure, but a *silent* one in the sense that mattered: nothing about
babble was wrong, and the number had been correct for months.

```rust
const CLASS_COUNT: usize = all_classes().len();
```

The same edit broke `names_and_literals_vary`, which sampled 8 programs and
asserted that generated integers vary. A shifted RNG stream put all 8 on the
same integer. The property is real; the sample was too small to be about the
sampler rather than about a lucky seed. Widened to 64. **Widen rather than pin a
seed** — the property should hold over the stream, not at a point.

## Retraining, and why the number went down

Corpus regenerated (1M programs, 86.9 MB) and drivel retrained at the same
52,000 steps: loss 2.278 against run 2's 2.26, 46.8k tok/s, 37.9 minutes.

| Measure | run 2 (older grammar) | now |
|---|---|---|
| Unconstrained parse, as sampled | 85.0% | **78.0%** |
| Unconstrained parse, complete items | 91.0% | **88.5%** |

**This is not a regression in the rig, and I cannot tell you what it is.** Three
things changed at once: Stitch gained two keywords (59 classes instead of 57 —
a larger grammar at unchanged capacity), the corpus was regenerated from
scratch, and the printer bug was fixed. Separating them needs another run. What
the number *does* establish is a baseline on the language as it stands, which
the recorded 91% had stopped describing.

I also nearly published a much worse version of this table. My first regeneration
used the default 2,000 steps, finished in 93 seconds, and produced a checkpoint I
was about to compare against a 52,000-step run.

### The ladder is a matrix, not a vector

This is the same trap as an [earlier finding](post-63-drivel-speaks-stitch.md)
one level up. Run 1 of drivel reached loss 1.93 and scored 0% parse; run 2
reached 2.26 and scored 91%, because 15% of run 1's tokens were a corpus
separator that was nearly free to predict. **Loss is not comparable across
corpora.**

Generalised: a checkpoint is not a rung, it is a rung times a set of variant
axes.

| Axis | Values today | Fixed by |
|---|---|---|
| **rung** | babble, drivel, quip, cliché, ballad, saga | `ModelConfig` |
| **corpus** | babble-generated, real, mixed | corpus manifest + mix ratios |
| **comments** | absent, input-only (loss-masked), full | tokenizer + loss mask |
| **objective** | next-token, FIM, mixture | training loop |

**A number is comparable only within a variant column.** The {1,3,10,30}M
scaling curve is a claim about one column, not about the ladder. Run 2 and this
run are two experiments, not two measurements of one thing.

Most cells are empty and some are uninhabitable: babble emits no comments, so
*(babble-corpus × input-only)* does not exist. And FIM — which the plan lists as
a metric the harness should compute — is a **training-time objective**, so
*(any rung × FIM)* cannot be evaluated on a checkpoint trained next-token-only.
Naming that as an empty cell is more honest than calling it a deferred metric.
The metric is ready; the cell is not.

## Costs, measured rather than guessed

~148 s for 74.5 KB, about 13 ms per decision. Every decision is 59 oracle
probes, each a parse of the whole prefix, so it is **quadratic in program
length** and `stim.st` (41 KB of the 58 that existed at the time) dominated.

Fine at this size, and it will not survive increment 2's corpus. The fix when it
bites is scoring per top-level item or incremental parsing. Recorded now so that
when the harness gets slow, nobody has to rediscover why.

## What is not built

The eval still refuses to print a **masked-NLL row for a trained checkpoint**,
and says so in its own output rather than printing something that looks
comparable and is not. That needs the class → vocab-token mask, which now has
[its own design note](../docs/grammar-mask-design.md), because the conversion
from a token distribution to a class distribution is exactly where a plausible
and meaningless number would hide.

The short version of that note: the obvious approach — sum model probability
over the tokens that could begin each class — **double-counts**, because `"<"`
begins both `Lt` and `Le`, and renormalising a double-counted total produces
something that sums to 1 and is a distribution over nothing. The way out uses a
measured fact: at the probe's vocab size, babble's lexicon saturates at 571
entries with exactly one token per whitespace lexeme, so the vocab is in
practice a *lexeme* vocab and each class maps to a set of complete-lexeme
entries.

That fact was measured on babble output, and increment 6 scores real Stitch,
where `operatorPending` will not be one token. So the note's first gate is a
read-only measurement of that assumption on the held-out corpus, before any code
— and if it fails, the design changes shape.

---

## Things worth remembering

- **A baseline nobody measured is not a baseline.** "Beat babble" survived in a
  plan for weeks and was clearable while losing to the trivial control.
- **The floor is the best zero-weight baseline, not the one you named.** Naming
  a specific rung as the floor is how a hollow win gets recorded as a real one.
- **Report the diagnostic column beside the number.** `free<50tok` is the
  difference between "the baseline is bad" and "the baseline is being asked a
  question it was never built to answer".
- **A parser branch needing more than one token of lookahead is a hole in the
  decode mask.** Its own oracle calls that prefix dead, and the construct
  becomes unreachable for every masked model at every scale.
- **The oracle is a property test on the parser's incrementality**, not only a
  decode mask. Scoring real human code is what makes it fire.
- **Over-parenthesising is not the safe direction.** The fuzzer caught my
  over-correction as fast as it caught the original bug.
- **A ritual in a human's head is a missing cache key.** If the answer to "how
  do we not get bitten again" is "remember to delete the file", that is a defect
  with a workaround, not a solution.
- **Widen the sample rather than pin the seed.** A property that holds at 8
  samples and fails at 8 different ones was never about the sampler.
- **Derive constants from the source of truth.** `CLASS_COUNT = 58` was correct
  for months and then was a compile-time panic.
- **A checkpoint is a cell, not a rung.** Comparing across a variant axis is the
  same category error as comparing loss across corpora, one level up.
- **Score the calibration control against something you cannot be wrong about.**
  A cheater must score zero; that assumes nothing about the question being asked.
