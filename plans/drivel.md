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
than **the best zero-weight baseline**, on real Stitch neither model was trained
on. Nothing else.

> Originally "lower than babble's". Increment 3 measured uniform-over-legal
> *beating* babble by 2.6 nats, so "beat babble" would have been clearable
> while losing to a model with no tables at all. The bar is **free-nll 2.742**
> (uniform, over 9 programs / 10,950 decisions). The bar is a property of the
> held-out set, so re-measure it whenever the corpus grows.

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

## Findings so far (measured, 2026-07-25)

**babble generation costs ~2 ms/program.** 500 programs/s in release, ~24
lexemes and ~83 bytes each. 30K programs = 2.48 MB / 722K lexemes / 60 s. Cheap
enough that corpus *size* is not a constraint on the probe — but not free, hence
the on-disk cache (`corpora/`, gitignored, with a `.manifest` recording seed,
count, and a fingerprint of babble's own output so a corpus that predates a
babble change is detected rather than silently trained on).

**Merges attach the *trailing* space** — `"let "`, `"prod "`, `"-> "` — because
babble renders space-separated. This is GPT-2's leading-space behaviour
mirrored, and it means lexeme atomicity must be measured *as the lexeme occurs*,
not in isolation. It also sharpens the probe-vocab warning below: the vocab is
shaped by babble's **renderer**, which emits neither newlines nor indentation, so
it is doubly wrong for real Stitch.

**`> =` is not a rendering bug — the space is load-bearing.** babble sampled the
`Gt` class then the `Assign` class, two separate legal choices. Written adjacent,
maximal munch would re-lex `>=` as a single `Ge` token, so the space is what
preserves the identity of the tokens the oracle actually approved. (Proof in the
same corpus: a walk that samples `Ge` emits `>= 3`.) The call-paren gotcha
wearing a different hat.

A **minimal-whitespace renderer** is the real fix for density and is cheap: for
each adjacent pair, try joining, re-lex, and keep the space only when the token
stream changes. Worth doing before Tier-0 output enters the *real* corpus, since
padded operators are a distribution the model would otherwise learn.

**Layout is absent because Stitch's grammar is whitespace-insensitive** — babble
has no reason to emit newlines or indentation, so it doesn't. The fix is not to
teach babble layout but to **pretty-print**: babble output parses by
construction, so parse it and render the AST.

**Shipped (2026-07-26) as `Layout::{Flat, Printed}`** on the back of `stitch`'s
new AST printer. `Printed` gives tight operators (`sum queue<value> = frame`
where `Flat` had `sum queue < value > = frame`), real newlines, and indentation.
Round-trip is guarded by comparing **ASTs** before and after printing, which is
what catches the maximal-munch hazard — printing `Gt` then `Assign` adjacently
would re-lex as `Ge` and silently change the program.

**`Printed` is now the default for training**, reversing the earlier "not on the
probe's path" call. That reasoning was right that `unconstrained-parse%` ignores
layout, and wrong about what else layout buys: `Flat` teaches a model babble's
*renderer* (space-padded operators that appear in no real Stitch), and it
contains no indentation at all — so it never exercises the GPT-2 pre-tokenization
rule, which exists precisely so an indent run can become one token. A corpus that
cannot test that decision is the wrong corpus to freeze a vocab against.

**BPE memorizes phrases when the vocab is large relative to the corpus.** At 768
merges over 400 programs the late merges are whole phrases (`"prod frame < "`,
`"let buffer = "`). Vocab size has to be sized against corpus size, and the
symptom is legible in the token list. **The freeze step must print the longest
learned tokens**: a vocab whose tail is `"prod frame < "` is overfit to its
corpus, and that is visible in ten lines of output but invisible in any summary
statistic. Cheap, and it is the kind of check that only gets built if it is
written down.

**Atomicity is frequency-relative, not absolute.** A lexeme the corpus barely
contains (`match`, `with`, `if`, `;` at 400 programs) is a *coverage* fact about
babble, not a tokenizer defect. The test asserts only over lexemes above an
occurrence threshold, which states the real contract and stops the assertion
drifting with corpus size. That babble under-produces those classes is itself a
production-coverage signal, and belongs in increment 2's report.

**The trainer's `O(target_size × corpus_len)` was a measured blocker — now
fixed.** The projected ~2 hours for a 4K vocab over 2.48 MB is **0.1 s**. The fix
was not the incremental-count rewrite that was planned, but **pre-tokenization
plus chunk-frequency aggregation** — the standard BPE trainer structure, which
turns the cost into `O(target_size × distinct_chunks)`. It also removed the
phrase-memorization pathology in the same change, since merges can no longer
cross a word boundary. The longest learned tokens are now clean lexemes
(`" contract"`, `" \"buffer\""`) where they used to be `"prod frame < "`.

This reverses the earlier "no pre-tokenization" decision. The original reasoning
— code wants indent runs as tokens — was right about the requirement and wrong
about the mechanism: GPT-2's rule (one leading space joins its word; longer
whitespace runs stand alone) satisfies it exactly, so `"\n   "` can still become
a token. Lexeme atomicity is now measured on `" let"`, not `"let "`.

**The probe corpus exists: 1M programs, 83 MB, 24.2M tokens, 7.7 minutes.**
`corpora/babble-0-1000000`. Generation is parallel across cores and
byte-identical to the sequential output regardless of core count (verified by
`cmp`, not asserted) — safe because a program's seed is a pure function of its
index. 24.2M tokens is the Chinchilla-ish 20 tokens/param for a 1M model, so the
probe is not data-limited.

**babble's lexicon saturates at 571 tokens — a hard ceiling on the probe
vocab.** Asked for 4096 over the 30K-program corpus, the trainer returns 571 and
stops: babble draws identifiers from a fixed wordlist and operators from a fixed
grammar, so once every distinct chunk is a single token there is nothing left to
merge. Confirmed by the compression figure — 721,876 tokens for 721,876
whitespace lexemes, exactly one token each.

Consequences, in order of importance:

1. **The frozen ladder vocab cannot be derived from babble at all.** This is a
   second, independent, and much harder reason than the distribution argument
   below: it is not that a babble-trained vocab would be *wrong*, it is that it
   cannot reach 2–4K entries.
2. **The probe's vocab is ~571 tokens**, which is fine and arguably good — a 1M
   model spends less of its budget on embeddings.
3. **Corpus volume, not vocab, is the probe's constraint.** 30K programs yield
   722K tokens; a 1M-param model wants ~20M. At 500 programs/s that is ~1M
   programs and ~33 minutes of generation — affordable, and now the sizing
   question for increment 5.

## The first trained drivel, and what it caught (2026-07-26)

**Run 1**: 52,000 steps, 35 min, loss 6.93 → 1.93, 50.4k tok/s. Unconstrained
parse rate: **0/200**.

The samples said why, immediately:

```
prod line()
contract span<port, buffer, buffer> { }
sum delta = task
use edge
```

That is competent Stitch. Every sample failed for one reason: it also emitted
`---`, the **corpus separator**. Training tokenized the corpus *file* rather than
the programs inside it, so `\n\x1e---\n` was in the stream and the model learned
it. The vocab was built from parsed programs and the token stream was not — two
paths that should have shared one.

**15% of the training corpus was separator** (26.75M tokens → 22.75M once fixed).
A sixth of the compute went into learning a delimiter.

Fixed by parsing once and feeding both paths, with programs joined by a **blank
line** — already what separates top-level items *within* a babbled program, so a
program boundary looks like every other boundary and there is nothing to learn
that is not Stitch.

**The evaluation earned its keep on its first run.** A bare `0.0%` would have
sent someone hunting through the backward pass, which was correct all along.
Printing three samples beside the number turned a mystery into an obvious bug in
seconds — worth keeping in every eval this ladder grows.

## Increment 5 result: drivel learns Stitch's grammar (2026-07-26)

**Run 2**, separator-free: 52,000 steps, 35.5 min, 50k tok/s, loss 6.93 → 2.26.

| Measure | Rate |
|---|---|
| **Unconstrained parse, as sampled** | **170/200 = 85.0%** |
| **Unconstrained parse, complete items** | **182/200 = 91.0%** |

The gap is the token budget: a fixed 96-token sample often stops mid-construct,
which is a property of the harness rather than the model. Both are reported so
neither can be quoted as the other. ~9% are genuine model errors.

**A 918K-parameter model, trained for 35 minutes, writes syntactically valid
Stitch ~9 times in 10 with no grammar mask.** Samples:

```
contract buffer { }
sum entry<price> = field(@) | depth
let price = not @ or ().token
ext sum count<total> = price
on task -> @ -> @ { }
```

**The answer to the ceiling probe is yes** — a model this small learns the
grammar when data is not the constraint. That was the gate on increments 2/3/6
being worth attempting, and it is now open.

### Loss went *up* between run 1 and run 2, and run 2 is far better

Run 1 reached 1.93; run 2 reached 2.26 and went from 0% to 91% parse rate. The
separator was cheap-to-predict filler — 15% of tokens, nearly free to model —
that dragged the average down. **Loss is not comparable across corpora**, only
within one. Worth remembering before any rung is compared to another on loss
alone; the ladder's eval gates are right to be defined on held-out task metrics
instead.

## Increment 3 result: the floor is not babble (2026-07-26)

**Status: COMPLETE.** The harness is `cram-eval/` (host-only, in-workspace,
in the gate), and `cargo xtask cram --eval` prints the scoreboard. The
standalone `parse-rate` bin is retired into it — a separate binary is how that
metric drifted into measuring one rung with no floor to compare it to.

Scored over every real `.st` file in the repo (9 programs, 74.5 KB, 10,950
decisions, 800 of them forced — includes the `examples/` corpus):

| rung | nll | free-nll | perplexity | free-nll over first 50 tokens |
|---|---:|---:|---:|---:|
| babble (tuned tables) | 5.001 | 5.395 | 220.4 | 2.619 |
| uniform-over-legal | 2.541 | 2.742 | **15.5** | 2.062 |

**Uniform-over-legal beats babble by 2.65 nats.** The plan assumed babble's
tuned tables were the floor; they are not, and the assumption was never
measured. Recorded consequences:

1. **The floor row is uniform, not babble.** Increment 6's win condition
   changes accordingly: drivel must beat **2.742**, not babble's 5.395.
   Clearing "the floor" while losing to a model with no tables at all would
   have been a hollow win, and the plan as written would have accepted it.
2. **The cause is regime, not tables-are-bad.** babble scores 2.619 over the
   first 50 tokens of a program and 5.395 overall; uniform is flat at
   2.062/2.742, as a rung with no notion of position must be. babble's
   finishing pressure saturates at `PRESSURE_CAP` on a long file, throwing
   nearly all its mass onto closers and `Eof` — a regime it *never generates
   in*, since its own walks stop by ~27 tokens. This is why the report carries
   the `free<50tok` column: without it the finding is a bad number with no
   explanation.
3. **This does not make babble a worse sampler.** It is a fine generator of
   short programs and a bad *model* of long files. Those are different jobs,
   and the eval measures the second one.

### The harness found a real oracle/parser disagreement on its first run — now fixed

`plans/lang/samples.st:79` writes `fold([:], …)`, the empty-map literal, and the
run reported the oracle rejecting a token a human actually wrote.

**Root cause: two tokens of lookahead in a one-token-at-a-time world.**
`parse_collection` recognised the empty map with
`peek() == Colon && peek_at(1) == RBracket`. That is correct on a whole program
and wrong for the continuation oracle, which probes by appending *one* token
plus `Eof` — so `peek_at(1)` was `Eof`, the branch was skipped, and the parse
error landed **on** the colon, which `oracle::admits` reads as "dead prefix".
The consequence was not cosmetic: `[:]` was **unreachable under a grammar
mask** — babble could never generate one, and no masked model could ever emit
one, however much training data contained it.

The fix commits on the single token (a map entry cannot start with its own
separator) and *demands* the `]` instead of peeking for it, moving the failure
one token later to where the oracle correctly reads it as "consumed it and
wanted more". Accepted language unchanged; only the error position moves.

**The general lesson, worth more than the bug:** a parser that needs *k > 1*
tokens of lookahead to commit has a prefix its own oracle calls dead. Any such
branch is a hole in the decode mask. This is the second time the oracle's
one-token contract has caught a latent assumption (the first was maximal munch
and the load-bearing space in `> =`).

Zero disagreements now, over 10,950 decisions.

### Retrained on the current language (2026-07-26, after `test` + `expect`)

Corpus regenerated (1M programs, 86.9 MB) and drivel retrained at the same
52,000 steps: loss 2.278 (run 2 reached 2.26), 46.8k tok/s, 37.9 min.

| Measure | run 2 (older grammar) | now |
|---|---|---|
| Unconstrained parse, as sampled | 85.0% | **78.0%** |
| Unconstrained parse, complete items | 91.0% | **88.5%** |

**Do not read this as a regression in the rig.** Three things changed at once
and the drop is not attributable to any one of them without a further run:
Stitch gained two keywords (`test`, `expect`, so 59 token classes instead of
57 — a larger grammar to learn at unchanged capacity), the corpus was
regenerated from scratch, and the printer's `expect` bug was fixed. What the
number does establish is a fresh baseline **on the language as it stands**,
which the recorded 91% no longer described.

The comparison is the ladder's own matrix rule (see the ladder doc) arriving in
practice: run 2 and this run are different *cells*, not two measurements of one
thing, because the corpus axis moved under them. Same trap as "loss is not
comparable across corpora", one level up.

### The corpus cache now sees the grammar, not just a sample of its output

The manual "delete the manifest after a printer change" ritual was a missing
cache key, and it cost a real run: a corpus generated before a printer fix looks
perfectly fresh to a digest that fingerprints three programs. Two changes, both
from failures that actually happened:

- **`Manifest::grammar_digest`** — FNV over every token class and its spelling.
  Deterministic, not sampled, so a keyword added, removed or respelled
  invalidates every cached corpus. Adding `test` changed babble's output for
  *every* seed (a 59th class shifts every draw); nothing in the old manifest
  would have noticed.
- **`PROBE_COUNT` 3 → 256.** The `expect` printer fix changed how ~1% of
  programs render; a 3-seed probe misses that ~97% of the time, 256 misses it
  ~8% of the time. Costs ~0.5 s on a cache *hit* against ~8 minutes to
  regenerate.

`FORMAT_VERSION` 2 → 3, so every manifest written before the grammar digest
existed is invalid by construction rather than by luck. What remains uncovered
is a printer change touching well under 1% of programs — a sample cannot close
that, and the honest fix there is still to delete the manifest.

### Cost, measured rather than estimated

**~138 s for 8,318 decisions — ~17 ms each**, and it is quadratic in program
length: every decision is 59 oracle probes, each a parse of the whole prefix,
and `stim.st` (41 KB of the 58 KB) dominates. Fine at 2K lines; **it will not
scale to increment 2's corpus**, and the fix when it bites is incremental
parsing or scoring per top-level item rather than per file.

### What is deliberately not here

- **No masked-NLL row for a checkpoint.** Scoring a *model* on the gate metric
  needs the class → vocab-token mask, and a real train/held-out split, which is
  increment 2's. The `--eval` output says so rather than printing a number that
  looks comparable and is not. The mask has its own design —
  [../docs/grammar-mask-design.md](../docs/grammar-mask-design.md) — because the
  conversion from a token distribution to a class distribution is where a
  plausible-but-meaningless number would hide, and because constrained decoding
  wants the same table.
- **FIM match**: an empty cell, not a deferred metric — see the ladder doc's
  matrix section. FIM is a training-time objective and no rung has been trained
  with it, so there is nothing to measure rather than something skipped.
- **Shape distance**: not built. The harness prints parse rate with samples
  beside it; shape statistics against the real corpus are worth having and are
  not on the gate's path.

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

### A comparison must fix the vocab, or report bits-per-byte

The section above says *which* vocab to freeze. This one says why two runs may
not each train their own, and it is a separate trap with a separate victim.

`xtask cram` trains a fresh probe vocab per run by default. Two arms that do so
tokenize the same held-out bytes into **different numbers of tokens**, so their
mean per-token NLL has a different denominator — the metric measures the
tokenizer as much as the model. Worse, it is biased in the direction that hides
it: the arm whose vocab was trained closer to the eval distribution needs fewer
tokens per byte, and a per-token mean over fewer, individually-harder decisions
is not obviously wrong-looking. Both arms report a plausible number and the wrong
one can win.

This is the same category error as *loss is not comparable across corpora* one
axis over, and it belongs in the
[ladder-is-a-matrix](../docs/generative-ladder.md) reading of a checkpoint: the
vocab is a variant column, not a free parameter.

Two ways out, and the batch9/batch10 experiment used the first:

- **Fix one vocab across every arm.** `--vocab-file` trains against a frozen
  vocab instead of a fresh probe. The batch10 arms all ran against the frozen
  2048-entry `corpora/kvetch-batch9.vocab`, which is the only reason A, B and C
  are comparable to each other and to the noise-floor reruns.
- **Report bits-per-byte**, which is denominator-independent and therefore
  comparable across tokenizations by construction.

Prefer both: the frozen vocab makes the comparison valid, bits-per-byte makes it
*checkable* by someone who does not know which vocab was used. A held-out NLL
quoted without naming its vocab is not a number anyone can reuse.

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
a project that names its model tiers after bad writing.

(An earlier draft held `kibitz` in reserve for the eval layer. When that layer
arrived it became **`cram-eval`** instead: the creative register belongs to
things with character — a rung, a sampler, a trainer — and a scoring harness is
infrastructure. `cram-corpus` assembles, `cram-eval` scores.)

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
- **`cram/`** — in-workspace, host-only. The training loop: hand-written backward
  pass, AdamW, batching, checkpoint export. Takes a `Rung` + a corpus, emits a
  checkpoint. It was to be *excluded* from the workspace to keep candle out of the
  gate; with no framework there is nothing to exclude, and being in-workspace means
  gradient checking runs in `cargo xtask test` like any other suite.
- **`cram-corpus/`** — in-workspace, host-only. Corpus assembly, the
  deterministic split, augmentation, the validator funnel, the per-batch report.
  Depends on `stitch` (parse + type-check) and `babble` (Tier-0 generation).
- **`cram-eval/`** — in-workspace, host-only. The scoring path: the `Predictor`
  trait every rung is measured through, masked NLL, the held-out loader, the
  `Generator` trait and parse rate. Links `kvetch-model` and **never `cram`**, so
  evaluating a checkpoint compiles no backward pass and no Accelerate binding.
- **Checkpoints are artifacts, not source.** Not committed, except one tiny drivel
  checkpoint enshrined as the deterministic eval fixture (the `panic-now` pattern
  the ladder doc names: feasibility artifact → permanent regression guard).

### The one abstraction the ladder actually needs

```
trait Predictor {
    fn weights(&self, at: Context<'_>, legal: TokenSet) -> Vec<(TokenClass, f64)>;
}
```

*(Built as `cram_eval::Predictor`, not `Rung` — `kvetch_model::Rung` is already
the checkpoint-config enum, and two `Rung`s in one workspace is exactly the drift
this trait exists to prevent. `Context` carries the prefix plus how far along and
how deep the walk is: babble's tables need both, and recomputing them per
position re-lexes the prefix every time, which is quadratic in program length.)*

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

### No framework: `cram` hand-writes the backward pass (decided 2026-07-25)

candle is **not** used. The whole pipe — forward, backward, AdamW, data loading,
checkpointing — is ours, in roughly 1000 lines. `llm.c` is the existence proof at
this scale, and the project's value here is understanding, not delivery.

**The seam that keeps this reversible is `Gemm`, not "the framework".** Over 95%
of training FLOPs are matmul, and the backward pass is *also* matmuls
(`dX = dY·Wᵀ`, `dW = Xᵀ·dY`); norms, softmax and SiLU are memory-bound and cheap
enough that our own code is fine. So one trait with one method carries the entire
performance story:

| Backend | LOC | Throughput | Role |
|---|---|---|---|
| `NaiveGemm` | ~15 | 1–2 GFLOP/s | reference; `no_std`; ancestor of the on-target kernels |
| `BlockedGemm` | ~100 | 50–100 GFLOP/s | portable fast path, zero deps |
| `AccelerateGemm` | ~20 | ~1 TFLOP/s | macOS AMX via `cblas_sgemm`; the training workhorse |
| candle | — | — | escape hatch, never load-bearing |

Trait-ing at the GEMM level rather than the framework level is what keeps a
framework a genuine swap-in without letting its design shape ours. "All backends
agree within tolerance" is a real test, and it replaces the
cross-implementation agreement check with something cheaper to keep honest.

### Backend throughput, measured (2026-07-25, M1 Max)

`cargo run --release -p cram --bin bench-gemm`, 2048-row multiplies at the
ladder's own projection shapes. GFLOP/s, best-of-N:

| shape | naive | blocked | accelerate |
|---|---:|---:|---:|
| drivel attn 128×128 | 3 | 131 | 1064 |
| drivel ffn-up 128×512 | 2 | 121 | 1748 |
| drivel ffn-down 512×128 | 2 | 118 | 1951 |
| ballad attn 384×384 | 2 | 126 | 2413 |
| ballad ffn-up 384×1536 | 2 | 135 | 2500 |
| ballad ffn-down 1536×384 | 2 | 147 | 2481 |

**The `Gemm` seam is worth ~500–1000×.** That spread is the whole justification
for the trait: the same model code runs at 2 GFLOP/s on the `no_std` reference
and 2.5 TFLOP/s on AMX, and the reference stays readable *because* it is not
where performance lives.

Shape sensitivity is real but mild and only bites at the bottom: drivel's
128×128 attention projection is the worst case at 1064, roughly 2.3× off the
2500 that ballad's larger shapes reach. Small `k` costs arithmetic intensity, as
expected — and it costs it where we can most afford it.

**Why this is affordable.** Training compute is `6ND`. The corpus is
data-limited at ~40M tokens (the ladder is not Chinchilla-limited), so `D` is
roughly constant across rungs and cost scales with parameters alone. At the
measured rates:

| Rung | FLOPs/epoch | 4-epoch run (GEMM-bound) |
|---|---|---|
| drivel 1M | 2.4e14 | ~11 min |
| quip 3M | 7.2e14 | ~25 min |
| cliché 10M | 2.4e15 | ~1.2 h |
| ballad 30M | 7.2e15 | ~3.3 h |
| saga 100M | 2.4e16 | ~11 h |

**Read those as lower bounds.** They are pure-GEMM projections, and the
non-matmul work — norms, softmax, attention itself, the optimizer — is
memory-bound and gets no acceleration at all. Amdahl applies: expect real
training at 50–70% of these figures, so drivel ~15–25 min and ballad ~5–7 h.

**This retires the earlier "DIY dies at cliché" exit criterion**, which assumed a
blocked-NEON backend and was stale by an order of magnitude — as was the ~1
TFLOP/s guess that replaced it, in the other direction. DIY reaches saga.

**The candle comparison is still outstanding** and remains the honest test of the
parity claim: AMX at 1–2.5 TFLOP/s against an M1 Max GPU whose fp32 peak is
~10 TFLOP/s. Plausibly within the 1.5× bar, plausibly not; unmeasured either way,
and it needs a network fetch to add the dependency.

**Above quip, compute is not the binding constraint anyway** — the real Stitch
corpus those rungs need does not exist yet. Training cost stops being the thing
that decides.

**The risk this trades into:** a hand-written backward pass is where bugs hide,
and there is no second implementation to agree with. The replacement is
**gradient checking** — analytic gradients against finite differences, per-op,
~30 lines. That is a *stronger* test than agreeing with candle, because it
validates against the mathematical definition rather than against another
implementation's choices.

**Inference needs none of this.** With a KV cache each decode step is a
matrix-*vector* product — memory-bound, ~2 MFLOP/token at drivel — so even
`NaiveGemm` yields ~500–1000 tok/s. A 200-token sample is ~0.2 s and 1000 sampled
programs is ~3 minutes. Increment 5's generation is not a performance problem;
only training is.

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

> **Measured, and it went the other way** — uniform beats babble by 2.6 nats;
> see the increment 3 result above. Two corrections to the paragraph as
> written. First, the control cannot be "babble with flat tables": flat bases
> still run through the pressure machinery (closers ×p, obligations ÷p³), so
> that is not a uniform distribution. The control is a separate `1/|legal|`
> predictor. Second, the calibration test cannot be "tuned beats uniform",
> since that is false — it is **a clairvoyant predictor scores ~0**, which
> checks the same thing (can the harness see signal) without assuming the
> answer to a question the harness exists to ask.

**GREEN**: the harness, plus the one new babble API it needs — **`p(class | prefix)`
over the legal set, not just `pick`**. `pick` must be shown to be a draw from
exactly that distribution (one test pinning the two together, the same
anti-drift discipline `admits_next`/`valid_next` already use in the oracle).

## Increment 4 — the rig can learn at all

**RED 1 — gradient checking.** Every backward op's analytic gradient matches a
finite-difference estimate of the same quantity, per-op rather than only
end-to-end. This is the increment's real deliverable: with no framework there is
no second implementation to agree with, so correctness comes from the
mathematical definition instead.

**RED 2 — backend agreement.** Every `Gemm` backend produces the same result
within tolerance for the same inputs. Cheap, and it is what makes the fast paths
safe to trust.

**RED 3 — the rig can learn.** Training 8 fixed sequences for N steps drives loss
below a threshold near zero. The classic overfit-one-batch check, promoted to a
unit test: a rig that cannot memorize 8 sequences is broken, and this is the only
place where that is cheap to tell.

**GREEN**: the `Rung` registry and forward pass, the `Gemm` trait and its
backends, the backward pass, `AdamW`, and the checkpoint format.

**Status: COMPLETE (2026-07-26).** Plus `cargo xtask train --rung drivel`:
push-button (corpus generated or reused, vocab trained, model trained,
checkpoint + TSV loss curve written) and self-reporting.

### Throughput, and what the self-report found

The run reports loss, smoothed loss, learning rate, gradient norm, tok/s,
elapsed and ETA every N steps. It earned that on the first run: **~3000 tok/s**,
extrapolating to ~9 hours against an estimate of 15–25 minutes. Three fixes, each
verified behaviour-preserving by an unchanged loss trajectory:

| Change | tok/s |
|---|---|
| first run | 3,000 |
| removed a `.to_vec()` and a loop-invariant in attention's inner loop | 4,136 |
| ran the batch's sequences concurrently | 26,063 |
| `Q·Kᵀ` and `P·V` as GEMMs instead of scalar loops | 37,055 |
| precomputed the `RoPE` rotation table | **55,945** |

Full 4-epoch run: **~29 minutes**, from ~9 hours.

The last one was the surprise and the largest single win. `rope_angle` called
`powf` per (position, head, pair) in both directions — but the frequency depends
only on the pair and the rotation only on `(position, pair)`, never on the head.
~2M transcendental calls per step became ~4K. **A table lookup beat every matmul
optimization**, which is not where anyone looks first.

Attention-as-GEMM under-delivered (1.4×, not the predicted 3×) because thread
parallelism was already absorbing that cost — the two overlap rather than
compound. Worth recording: the second optimization of the same bottleneck pays
much less than the first.
Six ops gradient-checked individually, then the whole model — every weight
against a finite difference of the real loss. The per-op and whole-model checks
catch different bug classes: an op can be individually correct while the
composition puts a gradient at the wrong offset, counts a residual fork once
instead of twice, or lets the tied embedding collect from only one of its two
uses.

Two invariants earned by measurement rather than assertion, both mutation-tested
by deleting the term and confirming the check fails:

- `RMSNorm`'s `1/rms` coupling term (fails at 40% error without it).
- Attention's softmax row-coupling `−Σ p·dp`.

Both are the terms a hand-derivation drops, and in both cases the wrong gradient
is *stable* — it trains, slowly, to the wrong place. Nothing but finite
differences would say so.

**Two anti-drift structures hold the whole thing together**, and both are the
same principle that put the oracle in `stitch`:

- `ModelConfig::layer_offsets` is the single source of weight-layout truth;
  forward reads through it and backward writes through it.
- `forward_with` is `trace_with(...).logits`, so there is exactly one forward
  implementation. A training-only copy would let training and serving drift —
  and gradient checking *cannot* catch that, since it validates backward against
  whichever forward it was handed. Both would be consistently wrong.

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
`cargo xtask clippy`. `kvetch-vocab`, `kvetch-model`, `cram-corpus` and `cram-eval` are
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
