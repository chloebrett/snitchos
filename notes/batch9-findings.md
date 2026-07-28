# batch9: what a 1000-candidate generation run taught us

Run: `qwen/qwen3.5-9b`, temp 0.7 / top_p 0.8 / top_k 20 / presence 0 / max_tokens 4096,
100 recipes × 10, guard on. Crashed at 973/1000 — the uncommitted incremental
`write_manifest` in `xtask-cram/src/generate.rs` is the only reason the verdicts survived.
**Commit that.**

23.5 hours wall clock, 3.09M model output tokens, 36.6 tok/s.

## The funnel

| stage | n | share |
|---|---|---|
| parse | 437 | 45% |
| tests (parsed + typed, some test failed) | 391 | 40% |
| type | 135 | 14% |
| ok (every test passed) | 10 | 1% |

batch5 (50 candidates, same model and settings) was parse 36% / tests 56% / type 6% / ok 2%.
**batch9 is worse**, and the reason is length, not the model.

## Finding 1 — parse death is a function of program length

Parse-death rate by decile of saved `.st` size:

| decile | ≤ bytes | parse | mean corrections |
|---|---|---|---|
| 1 | 5105 | 16% | 12.9 |
| 2 | 6238 | 22% | 17.0 |
| 3 | 7026 | 31% | 21.0 |
| 4 | 7777 | 34% | 24.6 |
| 5 | 8464 | 27% | 25.0 |
| 6 | 9334 | 39% | 26.7 |
| 7 | 10846 | 57% | 31.6 |
| 8 | 13068 | 56% | 33.2 |
| 9 | 17968 | 74% | 32.2 |
| 10 | 197373 | 92% | 37.5 |

Monotone, 16% → 92%. batch5's programs were half the size (1231 vs 2701 median model
tokens) and took 5.0 corrections each; batch9's take 26.2. The guard's per-rewind damage
compounds, so length is the independent variable that sets the yield.

## Finding 2 — the rewind eats a space

25 482 corrections. 69% discarded text that *started* with whitespace; only 20% of the
replacements start with whitespace. **49% of all corrections are a whitespace-to-
non-whitespace join.** The top pairs are the model re-emitting the same token minus its
separator:

```
663  ' if'    -> 'if'
482  ' let'   -> 'let'
349  ' ='     -> '='
302  'ext'    -> 'ext'
262  ' }'     -> '}'
213  '   if'  -> '  '        <- 3 spaces of indent become 2
171  '       []' -> '      ' <- 7 become 6
```

Cause: `run_once_corrected` resumes by sending the kept prefix as a trailing `assistant`
message. The prefill boundary loses one separator — either the chat template strips it or
the model will not open a continuation with a leading space. Either way, resuming a prefix
that ends mid-line reliably deletes one character of whitespace.

~2970 of those joins survive verbatim into the saved `.st` (~3 per file, present in 97% of
files). Most are indentation loss; a minority are real identifier glue (`ext` + `freeSpace`
→ `extfreeSpace`, `let` + `sum` → `letsum`), which is exactly the parse-error population:
83 × `unexpected token: If`, 18 × `unexpected token: Let`, 19 × `expected '(' after
function name`, 6 × `expected a declaration, found Ext`.

**Fix**: after a rewind, if the discarded text began with whitespace and the replacement
does not, re-insert the separator before splicing. Cheap, local, and it should move the
biggest single block of parse deaths.

## Finding 3 — smaller pathologies

- **199/973 hit the 4096-token cap.** 129 of the 437 parse deaths do not end in `}`, i.e.
  they are truncated rather than wrong.
- **45 files (5%) are degenerate loops** — a ≥6-character line repeated ≥15×. Worst is
  `195.st`: `|> unwrap` 1024 times. 4 of the 5 worst are `parse`. These are poison as
  training data whatever else we decide.
- **`extra_blocks > 0` on only 4 candidates** and zero `<think>` blocks — the prompt and
  `enable_thinking: false` are both working.

## Finding 4 — the recipe expansion added domains Stitch cannot express

Per-domain, 10 attempts each. Best: `soil moisture probe` 9/10 reaching tests,
`warehouse bin allocation` 8/10, `museum exhibit rotation` 8/10, `bird feeder log` 8/10.
Worst, all 0/10: `sudoku grid` (10/10 parse), `bowling scorecard` (10/10 parse),
`orienteering control points` (10/10 parse), `go territory scoring`, `darts scoring`,
`cribbage board`, `playlist shuffling`, `woodworking cut list`.

The dead domains all want indexed iteration over a grid or a running state machine — which
in a language with no loop keywords and no `xs[i]` becomes long recursive fold chains, i.e.
the long programs Finding 1 says will die. This is the same signal the uncommitted
`reference.md` addition ("Collections have NO methods and NO indexing", "`use List` is
required") is responding to.

## Finding 5 — the gate stages other than parse are informative

- type deaths: 28 undeclared effects, 24+ type mismatches, 13+ bad operator applications.
  Effect declarations are the single most common semantic miss.
- test deaths: median 1 passed / 5 failed; **174 of 391 had zero tests pass**. The model
  writes tests it cannot satisfy more often than it writes a nearly-right program.

## Token count

Tokenized with `checkpoints/drivel-9.vocab` (1024 entries), a faithful Python port of
`kvetch_vocab::{pre_tokenize, encode}`.

| corpus | files | bytes | tokens |
|---|---|---|---|
| batch1–8 | 101 | 453 884 | 317 336 |
| batch9 | 973 | 10 451 977 | 6 845 676 |
| **all `corpora/batch*`** | **1074** | **10 905 861** | **7 163 012** |

batch9 by gate stage:

| stage | files | tokens |
|---|---|---|
| ok | 10 | 111 273 |
| tests | 391 | 2 050 646 |
| type | 135 | 767 520 |
| parse | 437 | 3 916 237 |
| **parses cleanly (ok+tests+type)** | **536** | **2 929 439** |

Caveat: 1.52 bytes/token is poor compression — `drivel-9.vocab` was trained on babble, not
on this. A vocab trained on batch9 at 4–8k entries would land nearer 3–4 bytes/token and
the token count would fall by roughly half. **The number is only meaningful against a
named vocab.**

## Blockers to training now

1. **There is no way to train on a batch directory.** `xtask cram` calls
   `load_corpus(programs, layout)`, which generates or reuses *babble*. `--corpus-dir`
   (or equivalent) does not exist. This is the one hard blocker.
2. **No frozen vocab.** Every run trains a fresh probe vocab, so losses are not comparable
   across runs. Before a real training run, train one vocab on batch9 and freeze it.
3. **Decide what to include.** The 437 parse-dead files are 57% of the corpus *by tokens*
   and are ungrammatical Stitch by construction. Excluding them still leaves 2.93M tokens —
   about 6× the 500k target. Recommendation: exclude parse deaths, exclude the 45 degenerate
   files, keep `type` and `tests` deaths (they parse, and wrong semantics is still valid
   syntax to learn from).
4. **Held-out contamination is already handled** — `cram_eval::corpus::NOT_CORPUS` excludes
   `corpora/`, so `examples/` stays clean.
5. **Size ceiling.** 2.9–6.8M tokens feeds `drivel` (1M params) comfortably and `quip`
   (3M params) with epochs. `cliche` (10M params, ~200M tokens at Chinchilla) is out of
   reach. At 36.6 tok/s, generating 20× more corpus is ~470 hours — generation throughput,
   not the trainer, is what caps the ladder.

## Training results (2026-07-28)

The blockers above are cleared and `drivel` is trained. Everything below uses one frozen
vocab (`corpora/kvetch-batch9.vocab`, 2048 entries) and one frozen held-out set
(`corpora/heldout`, 116 programs / 302 985 tokens = 20% of the real files plus 20% of the
batch9 keep-set), so every number is comparable to every other.

### Choosing the vocab

`--save-vocab` over the training split, at four sizes:

| entries | training tokens | bytes/token | train time | drivel params |
|---|---|---|---|---|
| 512 | 1 771 233 | 2.05 | 5s | 853 120 |
| 1024 | 1 360 658 | 2.67 | 14s | 918 656 |
| **2048** | **1 120 837** | **3.24** | **33s** | **1 049 728** |
| 4096 | 956 158 | 3.80 | 65s | 1 311 872 |
| 8192 | 839 780 | 4.33 | 123s | 1 836 160 |

**2048.** Above it the embedding table starts to dominate a 1M-parameter rung — 8192 nearly
doubles the model — while each row gets less evidence: 2048 gives ~550 occurrences per
token, 8192 gives ~100. The longest-token report agrees: at 2048 the tail is indentation
runs and comment rules (which is what a code vocab *should* spend its tail on, and what
`pre_tokenize` keeps whitespace runs whole for), while at 4096 it already contains
`" reduceReduceReduceReduceRedu"` — a degenerate-repetition artifact from Finding 3 being
memorised as a token.

Note this vocab compresses this corpus at **3.24 bytes/token** against the 1.52 that
babble-trained `drivel-9.vocab` managed, which is why the token counts in the section above
are roughly halved everywhere below.

### The step sweep — where the clean corpus runs out

`drivel`, union of real Stitch + the batch9 keep-set (parse deaths dropped), 1.12M training
tokens:

| steps | tokens seen | epochs | train loss | held-out loss | wall |
|---|---|---|---|---|---|
| 3 000 | 6.1M | 5.5 | 2.772 | 3.326 | 124s |
| 10 000 | 20.5M | 18.3 | 2.226 | 3.123 | 413s |
| 15 000 | 30.7M | 27.4 | 2.109 | **3.084** (min 3.077 @ 12k) | 625s |
| 30 000 | 61.4M | 54.8 | 1.881 | 3.093 (min 3.078 @ 15k) | 1230s |

The held-out curve **bottoms out around step 12 000–15 000 and then turns**, while the
training loss keeps falling. That is the overfitting knee, and it is the first thing this
project has ever been able to see — before this run the loop reported training loss only.

### The finding: dropping the parse failures was the wrong call

Both runs below train for 15 000 steps against the *same* held-out set, so only the training
corpus differs:

| run | corpus | training tokens | epochs | held-out loss |
|---|---|---|---|---|
| `drivel-clean` | parse deaths **dropped** | 1 120 837 | 27.4 | 3.169 |
| `drivel-all` | parse deaths **kept** | 2 932 977 | 10.5 | **2.797** |
| `drivel-all-30k` | parse deaths kept | 2 932 977 | 20.9 | **2.688** |

Keeping the 437 parse-dead files is worth **0.37 nats** at equal step count, and 0.48 nats
once the bigger corpus is trained to convergence. The plan's recommendation — drop them,
they are ungrammatical by construction — was wrong, and the control run is the only reason
we know.

Why: at drivel's size the binding constraint is *volume*, not purity. 1.12M tokens is 27
epochs of memorising; 2.93M is 10. And the parse-dead files are not garbage — Finding 2
says they are long, mostly-correct programs carrying a handful of glued tokens, and
`run_once_corrected`'s own doc comment says as much ("overwhelmingly correct Stitch by
token"). The held-out set contains only clean programs, so this is not the model learning
to like broken text; it is genuinely better at clean Stitch.

`drivel-all-30k` is the checkpoint to keep. Its held-out loss had flattened by ~26k
(2.6947 → 2.6872 → 2.6879), so it is at or near converged for this corpus and this rung.

### Parse rate

`cargo xtask cram --eval --corpus-root corpora/heldout --samples 200`:

| rung | as sampled | complete items |
|---|---|---|
| babble (floor) | 200/200 = 100% | 100% — by construction, the mask guarantees it |
| `drivel-clean` | 47/200 = 23.5% | 62/200 = 31.0% |
| `drivel-all-30k` | 50/200 = 25.0% | 56/200 = 28.0% |
| `quip-all` | **59/200 = 29.5%** | 63/200 = 31.5% |

**Parse rate did not see the difference the held-out loss did.** 23.5% vs 25.0% at n = 200
is inside the sampling error; the held-out loss separates the same two checkpoints by 0.48
nats. This is the argument for the gate metric being NLL rather than parse%, made
concretely: a generative rate over 200 samples is too coarse to rank two rungs that are
genuinely half a nat apart.

For reference the floors' masked NLL on this held-out set is babble 5.2927 / uniform 2.6941
free-nll. **Do not compare that to the held-out losses above** — masked NLL is scored over
oracle-legal token *classes*, the training loss is unconstrained cross-entropy over the 2048
vocab. They are different quantities that happen to land near each other.

25% is well below the 91% that drivel-on-babble reached, and that is expected: babble is
generated from the grammar, so its programs are short, shallow and structurally uniform.
These are 96-token unconstrained samples in the presence of real Stitch's comment style,
generics and effect annotations. Samples read as plausible Stitch that loses its balance
partway through — the right failure mode for this size.

### quip: 3× the parameters buys 0.03 nats

`quip` (3.05M params, d 192 × 6 layers × 6 heads) on the same corpus, same vocab, same
held-out set, 20 000 steps:

| run | params | steps | training tokens | epochs | train loss | held-out | wall |
|---|---|---|---|---|---|---|---|
| `drivel-clean` | 1.05M | 15 000 | 1 120 837 | 27.4 | 2.109 | 3.169 | 623s |
| `drivel-all` | 1.05M | 15 000 | 2 932 977 | 10.5 | 2.189 | 2.797 | 623s |
| `drivel-all-30k` | 1.05M | 30 000 | 2 932 977 | 20.9 | 2.079 | 2.688 | 1255s |
| `quip-all` | 3.05M | 20 000 | 2 932 977 | 14.0 | 1.834 | **2.658** | 1608s |

quip wins, and by almost nothing — 0.030 nats for triple the parameters and 28% more wall
clock, and its held-out loss was still falling at the end (2.6699 → 2.6611 → 2.6575) so a
longer run would take it further. Parse rate moves more than the loss does: 25.0% → 29.5%.

**This is the data-starvation signal, and it is the most useful number of the day.** 2.93M
tokens is ~1 token per quip parameter against Chinchilla's 20. Scaling the rung is not what
this ladder needs next; corpus is. Throughput on that is the real constraint — at batch9's
36.6 tok/s, a corpus big enough to feed `cliche` is several hundred hours of generation, so
Findings 1–4 (yield per candidate) matter more than any training-side change.

### Half the training budget went on English prose

Comments are in the corpus verbatim — nothing strips them, and the samples make it obvious
that is where the capacity is going.

| corpus | comment bytes | comment tokens | lines |
|---|---|---|---|
| batch9 (all 973) | 44.2% | **47.3%** | 88 733 comment / 139 370 code / 36 706 blank |
| real `.st` | 37.8% | 38.3% | 1 625 comment / 4 277 code / 1 040 blank |

**Nearly half of every token this model saw was English.** Modelling English prose is not
something a 1M-parameter transformer can do — the samples are full of `getAmnestyPolicyToBmnesty`,
`puzzle.persudget(puzzle)`, "a rolling-recal days" — so that half of the budget buys almost
nothing, while competing for the same parameters as the grammar.

It also **inflates the parse rate**. Over 60 samples at 96 tokens (temperature 0.8):

- 20/60 parsed (33.3%),
- **5 of those 20 contain no code line at all** — a file of pure comments parses trivially,
- parse rate among samples that do contain code: **15/55 = 27%**,
- parsing samples average **67%** comment text; failing ones average 37%.

So the more of a sample is comment, the more likely it "parses". The headline 25–29.5%
figures are measuring grammar and comment-fraction together.

#### …and stripping them makes the model worse

`--strip-comments` drops `//` comments from both sides of the split (skipping `//` inside
string literals, and removing comment-only lines entirely so blank-line structure survives).
Two 30 000-step runs, same vocab, both scored against the **same comment-free held-out
stream** (`corpora/heldout-nocomment`, 187 781 tokens) so the comparison is exact:

| run | training tokens | epochs | train loss | held-out loss | parse rate |
|---|---|---|---|---|---|
| `drivel-comments` | 2 932 977 | 21.0 | 2.107 | **2.4497** | 50/200 = 25.0% |
| `drivel-nocomment` | 1 500 727 | 40.9 | 1.389 | 2.7192 (min 2.706 @ 18k) | 9/200 = 4.5% |

**Stripping comments costs 0.27 nats at predicting code** — which is now the only thing the
metric measures. Three things are going on:

- Halving the corpus costs more than focusing the budget gains. Same lesson as the
  parse-failure control and the quip run: at this size, volume is the binding constraint,
  and that has now been the answer three times.
- The stripped run memorises much faster — training loss 1.389 against the baseline's 2.107,
  with the held-out minimum arriving at 18k steps instead of 27k. Code alone is more
  predictable, so a fixed model runs out of corpus sooner.
- Plausibly, **the comments are load-bearing**. `// Calculate the remaining free space in a
  bin` sitting directly above `freeSpace(b: Bin) -> Int` is a free hint, and lexical overlap
  that blatant is usable even by a 1M model. This experiment cannot separate that from the
  volume effect; distinguishing them needs a run that keeps the comments but pads the corpus
  back to 2.93M some other way.

**The parse-rate column is not apples to apples** and should not be read as a 5× regression:

- At a fixed 96-token sample budget, a comment-free sample is *far more code*. The
  comment-free model has to keep ~96 tokens of syntax balanced where the baseline has to
  keep ~50 — a harder task, not necessarily a worse model. (`complete items` is much closer:
  22.0% vs 28.0%.)
- Stripping leaves slightly longer newline runs at program boundaries — blank lines go from
  13.9% to 20.8% of all lines, purely because code lines were removed, and the training-text
  `\n\n` join then sits on top of them. Prompted with `"\n"`, the stripped model burns
  budget on blank lines: one sampled program opened with thirteen. The corpus artifact
  itself is small (only 1% of blank runs are 3 or more) but it costs sample budget.

**Conclusion: comments stay in, and `--strip-comments` stays off by default.** They are not
dead weight paid for in entertainment — they earn their place on the gate metric.

### Caveats on these numbers

- **The held-out loss is a 64-window sample** (`--eval-batch`, ~8 192 of 302 985 tokens),
  fixed for a run so consecutive points differ by the model alone. It is reliable for
  comparing runs *on the same held-out stream* and is not an absolute: re-ordering the same
  held-out programs moved the reported loss by 0.085. Raise `--eval-batch` before quoting
  one of these as a standalone figure.
- The `drivel-clean-*` sweep rows used the stride split; `drivel-clean` / `drivel-all*` used
  the frozen `corpora/heldout`. The sweep's shape (where the knee is) is sound; its absolute
  values are on a different stream from the comparison table.
- `--eval` gives a trained checkpoint **no** masked-NLL row. That needs the class→vocab
  decode mask (increment 6). Parse rate and held-out loss are what we have.

### Still open

- The 45 degenerate files (Finding 3) were left in. Only ~5 of them survive into the
  keep-set, but all 45 are in `drivel-all`'s corpus, and one of them reached the 4096-entry
  vocab as a token. Filtering them is untested.
- The vocab is not frozen in the wire-law sense — it sits in `corpora/`, uncommitted.
  Promoting it into `kvetch-vocab/assets/` with a digest test is a deliberate call.
- `xtask/src/main.rs`'s `the_derived_plan_matches_the_previously_hardcoded_set` fails on a
  clean tree, and did before any of this work: the derived mutation plan has picked up
  `cram`, `cram-corpus`, `cram-eval`, `cram-gen`, `kvetch-model`, `kvetch-vocab` and
  `xtask-cram` since the characterisation list was written. Enrolling seven crates in the
  mutation gate is the deliberate act that test exists to force, so it was left alone.
- `checkpoints/drivel-0.tsv` — the 52 000-step reference curve quoted in the timings above —
  was overwritten by a 20-step smoke run before `--name` existed. The numbers in this
  document were taken from it beforehand; the file is gone.

## What to do before batch10

- Commit the incremental `write_manifest`.
- Repair the rewind splice (Finding 2).
- ~~Cap program size in the prompt~~ — **done**, see below.
- ~~Drop or rewrite the ~8 domains that need indexed grid iteration~~ — deliberately
  **kept**, see below.
- Record `abandoned` in `CandidateRecord`; it is currently computed and thrown away, so
  "the guard gave up" is invisible in the manifest.

## The recipe sheet for batch10

Recipes are now **one sheet per batch**, in `cram-gen/assets/recipes/`, selected with
`cargo xtask cram --gen --recipes <name>` and recorded in the manifest header.
`batch9.toml` is the frozen 100 that produced this corpus — including the wording its
briefs were rendered with, so everything above still corresponds to something.
`batch10.toml` is the new default:

- **500 domains, 1000 rows.** Each domain is asked for twice at *different* crossings.
  batch9 asked each of its 100 domains ten times at the identical crossing, so nine of
  every ten programs varied only by sampling noise.
- **Pass-major flattening.** All 500 first crossings, then all 500 second ones — a
  500-candidate run covers every domain rather than half of them twice.
- **Response to Finding 1**, in two parts. The size mix goes from 24/56/20
  small/medium/large to 500/462/38, and the brief's closing sentence changes from "if the
  program naturally wants to be bigger, let it be" to "a longer program is not a better
  one — cover the core computation, test it, and stop". The cap stays in *declarations*,
  never lines: `plans/corpus-mvp-spike.md` Findings 004 is that a model cannot count lines
  and wrecks working code trying to.
- **Shapes spread**: 62% module becomes 45%, and `script` goes from 5% to 16%. Which
  exposed a prompt bug worth its own line: every brief opened "Write a `<domain>` module"
  and then said "Shape: a script" underneath. Survivable while most of the sheet really
  was modules; wrong more often than right once the shapes spread. batch10's briefs open
  with the shape's own noun, batch9's still say module.
- **The eight zero-yield domains are kept.** Finding 4 says they die because indexed grid
  iteration becomes a long recursive program, and Finding 1 kills long programs. Asking
  for a small one is a different experiment, and their batch10 yield is what settles it.
- `CandidateRecord` now carries `size` and `shape` beside `domain`. Per-domain analysis
  was enough for batch9 because the crossing never varied within a domain; it is not
  enough here.
