# batch11: where volume stops paying, and what the exemplars are worth

Two questions, measured the same way [batch10](batch10-training-findings.md) was.

1. batch10 finished to 1000 candidates and batch11 landed — roughly +2.4M tokens
   against a 4.3M-token corpus. Does volume keep paying at ~6.7M tokens, or has it
   saturated?
2. `examples/stitch/` is 30 hand-polished gold programs sitting inside every arm as
   part of `--real-root`. [Stitch 18](../posts/stitch-18-a-binding-is-not-a-boundary.md)
   ends on "what they're worth to an actual model is the next measurement". This is
   that measurement.

Every run is `drivel`, 30 000 steps, against the frozen `corpora/kvetch-batch9.vocab`
(2048) and the frozen 116-program `corpora/heldout`, `--eval-batch 1024`. Same
protocol as the batch10 note, so every number below is comparable to every number
there.

## The control reproduces exactly

Arm B (real + batch9 + batch10-snap) was re-run before anything else, because the
KV-cache work landed in `kvetch-model` after the batch10 numbers were taken and a
changed forward pass would invalidate the whole comparison.

It reproduces **bit-for-bit**: 4 318 391 tokens (identical corpus), 2.5584 at step
30 000, and all 31 rows of the published curve match on loss, smoothed loss,
learning rate and gradient norm. Training is deterministic across those commits, and
`--eval-every` does not perturb it — the held-out pass discards its gradient and
draws from a separately seeded window batch.

## The corpus

batch10 and batch11 were generated with **identical settings and the same recipe
sheet** (`recipes=batch10`, 1000 recipes over 500 domains, `correct=8`,
`max_bytes=12000`, same model and sampling). batch11 is not a better corpus or a
different corpus — it is *more of the same corpus*. That makes Task 1 a clean volume
experiment rather than a generator comparison.

| | batch9 | batch10-snap | batch10-full | batch11 |
|---|---|---|---|---|
| programs | 973 | 712 | 1000 | 910 |
| tokens (frozen 2048 vocab) | 3 131 032 | 1 387 041 | 2 008 753 | 1 792 801 |
| bytes/token | 3.34 | 3.14 | 3.15 | 3.14 |
| median bytes | 8 467 | 5 205 | 5 516 | 5 370 |
| p90 / max bytes | 18 191 / 197 373 | 12 001 / 18 301 | 12 001 / 24 509 | 12 001 / 28 029 |
| parse deaths | 45% | 15% | 15% | **14%** |
| degenerate files | 45 | 5 | 7 | **4** |
| comment share (tokens) | — | — | 46.4% | 45.6% |
| generation wall | 23.5 h | — | 16.3 h | 14.9 h |

Parse-death rate is quoted as a fraction of *written* candidates, the convention the
batch10 note used. Degenerate is the batch9 findings' definition — a ≥6-character
line repeated ≥15 times — and reproduces that note's counts exactly (45 and 5) on
the two corpora both notes cover.

**Leak check.** batch11 reuses batch10's recipes over 500 domains, so it can produce
a program on the same brief as a held-out one. Over all pairs against the 116
held-out programs:

| corpus | programs | max line-level Jaccard | pairs ≥ 0.20 |
|---|---|---|---|
| batch11 | 910 | **0.0588** | 0 |
| batch10-full | 1000 | **0.0672** | 0 |
| batch9 *(calibration)* | 973 | 1.0000 | 108 |

Nothing in either new corpus is a near-copy. batch9's 108 exact matches are the
held-out programs themselves still sitting in their source directory, which is the
instrument confirming it can see a leak — and confirming that the trainer's
content-keyed exclusion is what removes them (it reports 117 dropped).

## The noise floor

Every arm was run at seed 0 and seed 1. The seed reshuffles weight init, training
batch order **and** the held-out eval windows, so a seed spread is *total* spread —
an upper bound, which is the conservative direction.

| arm | seed 0 | seed 1 | spread |
|---|---|---|---|
| A *(batch10 note)* | 2.6689 | 2.6851 | 0.0162 |
| C *(batch10 note)* | 2.6601 | 2.6462 | 0.0139 |
| B | 2.5584 | 2.5627 | 0.0043 |
| E | 2.5639 | 2.5682 | 0.0043 |
| F1 | 2.5798 | 2.5954 | 0.0156 |
| D | 2.5309 | 2.5398 | 0.0089 |

Pooled over six arms the mean spread is **0.0105 nats**, consistent with the
established σ ≈ 0.013. Every marginal claim below is measured against **σ = 0.013**,
the established figure, because it is the more conservative of the two.

### But the comparisons here are *paired*, and pairing is much quieter

Two arms at the same seed share weight init and share the held-out eval windows.
Only the training token stream differs, so seed-level common factors cancel. The
evidence that this matters is arm E against arm B:

| | seed 0 | seed 1 |
|---|---|---|
| E − B | **+0.0055** | **+0.0055** |

The same delta to four decimal places at two independent seeds, on a metric whose
*marginal* spread is 0.0043–0.0162. A paired delta is resolvable well below the
0.03 that a single unpaired pair needs. Every finding below is therefore quoted as a
paired mean across two seeds, with the marginal-σ comparison given alongside so it
cannot be read as a stronger claim than it is.

## Task 1 — volume has largely stopped paying

| arm | corpus | tokens | epochs | seed 0 | seed 1 |
|---|---|---|---|---|---|
| **A** *(batch10 note)* | real + b9 | 2 931 385 | 20.96 | 2.6689 | 2.6851 |
| **B** | real + b9 + b10-snap | 4 318 391 | 14.23 | 2.5584 | 2.5627 |
| **E** | real + b9 + b10-**full** | 4 940 103 | 12.44 | 2.5639 | 2.5682 |
| **D** | real + b9 + b10-full + **b11** | 6 724 381 | 9.14 | **2.5309** | **2.5398** |

Curves at the shared eval points:

| step | A | B | E | D |
|---|---|---|---|---|
| 3 001 | 3.2727 | 3.1702 | 3.1973 | 3.2217 |
| 9 001 | 2.8953 | 2.7885 | 2.8027 | 2.7855 |
| 15 001 | 2.7731 | 2.6781 | 2.6772 | 2.6482 |
| 21 001 | 2.7147 | 2.5972 | 2.6084 | 2.5782 |
| 30 000 | 2.6689 | 2.5584 | 2.5639 | **2.5309** |

| effect | seed 0 | seed 1 | paired mean | vs σ=0.013 |
|---|---|---|---|---|
| **total (D − B)** | −0.0275 | −0.0229 | **−0.0252** | 1.9σ marginal, both seeds same sign |
| batch10 completion (E − B) | +0.0055 | +0.0055 | **+0.0055** | 0.4σ — nothing |
| batch11 (D − E) | −0.0330 | −0.0284 | **−0.0307** | 2.4σ |

**+55.7% more corpus bought 0.025 nats. +47% bought 0.111 last time.** Normalising
for the log does not rescue it: 0.286 nats per log-unit of corpus from A→B, 0.062
from B→D. A 4.6× collapse in return.

The two increments are not equal contributors, and neither is individually large:

- **Finishing batch10** (712 → 1000 candidates, +622k tokens, +14.4%) bought
  **nothing** — +0.0055, the wrong sign, at both seeds. The 288 candidates that
  arrived after the mid-generation snapshot are, as training data, free of charge in
  both directions.
- **batch11** (+1.78M tokens, +36%) bought −0.031, and D beats E at every eval point
  from step 9001 onward.

Per million tokens: A→B bought 0.080 nats/Mtok; B→D buys 0.011. **Marginal corpus is
worth about a seventh of what it was worth one batch ago.**

The cost side follows directly. batch11 cost 14.9 hours of generation for 0.031
nats. At that rate the next 0.03 nats is another day of wall-clock, and the one
after that is more — which is a different conclusion from the batch10 note's
"candidates per hour beats yield per candidate". That advice was right at 2.9M
tokens. At 4.9M it no longer buys anything worth the clock.

## Task 2 — swapping the exemplars out costs ~0.022 nats

The naive ablation (drop `examples/stitch/`, read the delta) moves two variables: it
removes the exemplars *and* ~195 KB of volume, and volume is exactly what dominates.
So this is designed the way arm C was — the exemplars are **replaced** by an equal
token count of generated corpus, and the arms differ only in what the tokens are.

Two facts shaped the design:

- **Six of the thirty exemplars are already in the frozen held-out set**
  (`bank`, `elo`, `interval`, `lru`, `regex`, `template`), so only **24** ever reach
  training: 154 160 bytes, 57 408 tokens, **1.16% of arm E's corpus**.
- The running-ratio sampler that built `batch9-half` systematically prefers small
  files — it keeps a byte budget, so a large file rarely fits. Its slice has
  batch11's recipes but not batch11's size distribution. So **two** independent
  replacements were drawn, bracketing that choice.

| | programs | tokens | vs exemplars | comment share |
|---|---|---|---|---|
| exemplars-24 | 24 | 57 408 | — | 36.3% |
| **b11-stride** (every 30th file, size-unbiased) | 31 | 58 107 | +1.2% | 43.2% |
| **b11-slice** (running-ratio, size-biased) | 54 | 56 613 | −1.4% | 36.8% |

The arms land within **0.014%** of each other on total tokens — E is 4 940 103 and
F1 is 4 940 802, a 699-token difference that is exactly the slice-minus-exemplar
delta. All three run 12.44 epochs.

| arm | real side | replacement | tokens | seed 0 | seed 1 |
|---|---|---|---|---|---|
| **E** | all 38 (24 exemplars train) | — | 4 940 103 | **2.5639** | **2.5682** |
| **F1** | 8 non-exemplar | b11-stride | 4 940 802 | 2.5798 | 2.5954 |
| **F2** | 8 non-exemplar | b11-slice | 4 938 226 | 2.5873 | — |

| step | E | F1 | F2 |
|---|---|---|---|
| 3 001 | 3.1973 | 3.2500 | 3.1136 |
| 9 001 | 2.8027 | 2.8190 | 2.8094 |
| 15 001 | 2.6772 | 2.7005 | 2.6919 |
| 21 001 | 2.6084 | 2.6195 | 2.6252 |
| 30 000 | **2.5639** | 2.5798 | 2.5873 |

| effect | seed 0 | seed 1 | paired mean |
|---|---|---|---|
| F1 − E | +0.0159 | +0.0272 | **+0.0216** |
| F2 − E | +0.0234 | — | +0.0234 |

**All three swap measurements are the same sign**, across two seeds and two
independently drawn replacement slices. Swapping the 24 exemplars out costs about
**0.022 nats**. F1 − F2 is 0.0075, which is the same-seed slice-choice noise — about
half the cross-seed spread, as it should be.

Against σ = 0.013 a single one of these is 1.2–1.8σ and **not quotable on its own** —
which is why the design has three of them and two seeds.

### What that is worth per token — and why the ratio is a ceiling

The swap measures a **difference**, not an absolute: exemplar-minus-generated ≈
0.022 / 0.0574M = 0.38 nats/Mtok. Getting the exemplar's own per-token value means
adding back the generated baseline at this corpus size. Fitting loss ≈ a − b·ln(N)
to E → D gives b ≈ 0.0995, so the *local* rate at 4.94M tokens is 0.020 nats/Mtok —
not the 0.0172 average over the whole E → D step. That puts exemplars near 0.40
against 0.020, so **~20× per token** (~26× per byte, since exemplars tokenize denser
at 2.69 bytes/token against 3.14).

Three reasons that number is a ceiling rather than an estimate:

- **The held-out confound could account for all of it.** See Caveats — 4.29% of
  held-out bytes are exemplars, and this measurement cannot separate "exemplars
  teach Stitch" from "exemplars teach exemplars". If the gain is concentrated on
  those six programs the true ratio is near 1×.
- **It is a local derivative at 1.16% of corpus.** The 200th hand-polished program
  would not be worth what the 1st was — diminishing returns apply to exemplars too,
  for exactly the reason Task 1 just demonstrated for generated corpus. This does
  not license "hand-write the corpus".
- **In absolute terms batch11 still delivered more** — 0.031 nats against 0.022. Per
  token the exemplars win by an order of magnitude; per *hour spent* the two are
  probably comparable, and the exemplars' real edge is that the same session also
  produced 279 native tests and several genuine interpreter bugs, which generated
  corpus does not.

So this does not overturn the four consecutive "volume beats purity" findings. 24
programs cannot *substitute* for 1.8M tokens. What they do is return far more per
token than anything else in the corpus — if the confound survives testing.

The confound runs *against* the result, which is worth stating: F1's slice carries
43.2% comment tokens against the exemplars' 36.3%, and comments are known to help
(stripping them cost 0.27 nats). F1 had the comment advantage and still lost.

## Caveats

- **Six of the 116 held-out programs are exemplars — 4.29% of held-out bytes.** The
  exemplar arm therefore gets in-distribution practice for a slice of what it is
  scored on. For the whole +0.022 to be this confound, the model would have to be
  0.51 nats better on those six programs alone. **A deconfounding pair (G/H) against
  a 110-program exemplar-free held-out set was designed and launched but not
  finished — see Open below. Until it lands, the Task 2 magnitude is a ceiling, not
  a point estimate.**
- **The held-out set is batch9-flavoured** — 108 of 116 programs are batch9, 8 are
  real, and it contains no batch10 or batch11 at all. So Task 1 measures "does more
  batch10/11 help on batch9-like data", the conservative framing, and Task 2's gate
  metric is 93% generated Stitch.
- **A paired delta is not a marginal delta.** The pairing argument rests on E − B
  landing at +0.0055 at both seeds; that is strong evidence but it is n=2. The
  marginal-σ figures are given beside every claim for exactly this reason.
- **F2 has no seed-1 run.** F1 does, and the two agree in sign at seed 0.
- **Nothing has converged.** D moves 0.004 over its last 3k steps and is the
  steepest of the arms, so a longer run would more likely widen D − E than close it.
- **Parse rate is not reported.** The batch9 and batch10 notes both established it
  cannot resolve checkpoints half a nat apart, and it is partly measuring comment
  fraction ([post 77](../posts/post-77-the-number-that-could-not-see-it.md)). Held-out
  NLL is the gate metric.
- **`cargo xtask test` has one pre-existing failure on a clean tree**
  (`mutant_plan_tests::the_derived_plan_matches_the_previously_hardcoded_set`) —
  unrelated to this work, predates it.

## Open

The deconfounding pair, ready to run — the corpora are built and the leak check
covers them:

```
cargo xtask cram --real-root . --batch-dir corpora/batch9 --batch-dir corpora/batch10 \
  --held-out-root corpora/heldout-noex --vocab-file corpora/kvetch-batch9.vocab \
  --steps 30000 --eval-every 3000 --eval-batch 1024 --seed 0 --name drivel-G-ex-noexheld
cargo xtask cram --real-root corpora/real-noex --batch-dir corpora/b11-stride30 \
  --batch-dir corpora/batch9 --batch-dir corpora/batch10 \
  --held-out-root corpora/heldout-noex --vocab-file corpora/kvetch-batch9.vocab \
  --steps 30000 --eval-every 3000 --eval-batch 1024 --seed 0 --name drivel-H-swap-noexheld
```

...and the same two at `--seed 1`. G trains on all 30 exemplars (only 2 real
programs are held out now); H swaps them for `corpora/b11-stride30`, 35 programs /
71 001 tokens against the 30 exemplars' 72 216 (−1.7%). Verified corpus sizes:
G 4 954 911 tokens, H 4 950 161 (0.10% apart). **G − H is comparable to F1 − E as a
delta; the absolute losses are not comparable to anything else here, because the
held-out set is different.**

## Artifacts

Checkpoints and curves in `checkpoints/` (gitignored): `drivel-B-repro`,
`drivel-B-repro-s1`, `drivel-E-b9b10full`, `drivel-E-b9b10full-s1`,
`drivel-F1-stride`, `drivel-F1-stride-s1`, `drivel-F2-ratio`,
`drivel-D-b9b10b11`, `drivel-D-b9b10b11-s1`.

Derived corpora in `corpora/` (gitignored, all reproducible):

| | what |
|---|---|
| `real-noex/` | the real corpus mirrored, `examples/stitch/` removed — a mirror rather than a flat copy so `--real-root` walks it in the same sorted order |
| `exemplars-24/`, `exemplars-30/` | the exemplars that train, and all thirty, for token measurement |
| `b11-stride/` | every 30th batch11 file, size-unbiased, matched to exemplars-24 |
| `b11-slice/` | running-ratio slice, size-biased, matched to exemplars-24 |
| `b11-stride30/` | every 26th batch11 file, matched to exemplars-30 |
| `heldout-noex/` | the frozen held-out set minus its six exemplars (110 programs) |

Token counts were measured with `cargo xtask cram --steps 1 --eval-every 0
--held-out-every 0 --vocab-file corpora/kvetch-batch9.vocab --batch-dir <dir>`,
reading the `tokens` line. `--held-out-every 0` matters: without it the probe
silently splits off a fifth and undercounts.

**`drivel-D-b9b10b11` (2.5309) is the best drivel checkpoint this project has
produced**, against `drivel-B-b9b10`'s 2.5584. It is *not* promoted to the embedded
checkpoint — `drivel-b9b10-30k` is committed, itest-asserted byte-exact and embedded
in a booting guest, and changing it is a separate deliberate call.
