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

One confound runs *against* the result, which is worth stating: F1's slice carries
43.2% comment tokens against the exemplars' 36.3%, and comments are known to help
(stripping them cost 0.27 nats). F1 had the comment advantage and still lost.

### The one that runs *for* it — and does not survive testing

Six of the 116 held-out programs **are** exemplars, 4.29% of held-out bytes. So arm E
gets in-distribution practice for part of what it is scored on, and the measurement
above cannot separate "exemplars teach Stitch" from "exemplars teach exemplars".

Arms G and H test it directly, against `corpora/heldout-noex` — the frozen set with
its six exemplars removed, 110 programs. G trains on all 30 exemplars (only 2 real
programs are held out now), H swaps them for `b11-stride30`, 35 programs / 71 001
tokens against the exemplars' 72 216 (−1.7%). Corpora are 4 954 911 and 4 950 161
tokens, 0.10% apart.

| arm | real side | replacement | seed 0 | seed 1 |
|---|---|---|---|---|
| **G** | all 38 (30 exemplars train) | — | **2.5388** | **2.5000** |
| **H** | 8 non-exemplar | b11-stride30 | 2.5472 | 2.5278 |

| effect | seed 0 | seed 1 | paired mean |
|---|---|---|---|
| **H − G** (exemplar-free held-out) | +0.0084 | +0.0278 | **+0.0181** |

**The effect survives.** Removing the held-out exemplars leaves 84% of it — +0.0181
against the confounded +0.0216 — so the confound is worth roughly 0.0035 nats, a
sixth of the total, not the whole thing. Both seeds keep the sign.

Absolute losses here are **not** comparable to E/F1/F2 or to anything else in this
note; the held-out set is different. Only the G − H delta is.

### What that is worth per token

The swap measures a **difference**, not an absolute: 0.0181 / 0.0722M = 0.25
nats/Mtok. Getting the exemplar's own per-token value means adding back the generated
baseline at this corpus size. Fitting loss ≈ a − b·ln(N) to E → D gives b ≈ 0.0995,
so the *local* rate at 4.94M tokens is 0.020 nats/Mtok — not the 0.0172 average over
the whole E → D step. That puts exemplars near 0.27 against 0.020:

| measurement | held-out | per-token multiple |
|---|---|---|
| F1/F2 − E | includes 6 exemplars | ~20× |
| **H − G** | exemplar-free | **~13×** |

**Take the ~13×.** The confounded arms flatter the exemplars, exactly as expected.

Three things it still does not license:

- **It is a local derivative at ~1.3% of corpus.** The 200th hand-polished program
  would not be worth what the 1st was — diminishing returns apply to exemplars too,
  for exactly the reason Task 1 just demonstrated for generated corpus. This is not
  an argument for hand-writing a corpus.
- **In absolute terms batch11 still delivered more** — 0.031 nats against 0.018. Per
  token the exemplars win by an order of magnitude; per *hour spent* the two are
  probably comparable, and the exemplars' real edge is that the same session also
  produced 279 native tests and several genuine interpreter bugs, which generated
  corpus does not.
- **This pair is the noisiest in the note.** G's own seed spread is 0.0388 — the
  exemplar-free held-out set is a smaller, higher-variance metric. The paired
  standard error on H − G is ≈ 0.010, so +0.0181 is ~1.9σ *on its own*. It is
  believable because it is the fifth same-sign measurement, not because this pair
  alone settles it.

So this does not overturn the four consecutive "volume beats purity" findings. 24
programs cannot *substitute* for 1.8M tokens. What they do is return roughly an order
of magnitude more per token than anything else in the corpus.

## What the checkpoints actually emit

Recorded because a bare loss number cannot be sanity-checked, and because the
comparison below is a worked example of an eyeball test getting the ranking wrong.

Twenty unconstrained 96-token samples per checkpoint, same seeds, same tiny eval
root, `cargo xtask cram --eval --checkpoint <ck> --eval-vocab <vocab> --samples 20`.
The three shown per model are seeds 0–2, i.e. the first three of the twenty, not a
selection.

| checkpoint | held-out NLL | parse rate (n=20) |
|---|---|---|
| `drivel-all-30k` *(embedded)* | 2.688 | 25% |
| `drivel-B-b9b10` *(previous best)* | 2.5584 | **15%** |
| `drivel-D-b9b10b11` *(new best)* | **2.5309** | 30% |

**`drivel-D-b9b10b11`** — seed 1, which parses:

```
// Bool to find the specific amount that has not. Returns a Result containing a list of spops.
contract HandarRequest {
    hasOccupiedDate(Str) -> Bool
    hasOccupied() -> Bool
    unwrap()
    credit(freeStep) -> List<LoanStep>
}
```

seed 0, which does not — note the collapse in the last `expect`:

```
test "calcCalcProration returns no change" {
    expect calcCalcProration(calcAmount(1000, 8), 10, 100) == 50
}

test "calcProration handles dm from change after period duration" {
    expect calcCycle(1000, 10) == 100
    expect calcCalcVolume(p, 0).unwrap().itillCycle(1000, 80, 5Selied.0)) == 30
}
```

seed 2, which runs out of budget mid-declaration rather than emitting anything wrong:

```
// --- Types ---

// A list of items checking parties.
// Remove squares used in inventory.
ext prod Peries(
    ext items: List<Int>
)

// --- Core Logic ---

// The function returns a list of items gets all inventory size.
ext dayBy(xs: List<Perment>) -> List<Item> =
```

**`drivel-B-b9b10`** — seed 1 and seed 2:

```
// The number of library month state.
// Uses the separate must be fixed 500 and returns emptyNtempted *maid* who are consistent.
ext prod Schedule(
    ext timestamp: Int,
    ext name: Str,
```
```
// PoolOf's ownerQiages.
ext ownerQiagesALELONDOMALDOLDOW. (which monents.)
    // Check if the person says "YYYYYMAL by B", mophat meStud1: "WA", lowerName: "D")
```

**`drivel-all-30k`** — seed 1 and seed 2:

```
// Check if a list of metrics fits largers for a specific page book.
ext hasPage(broodLast: List<Domino> = {
    if anyBroodLast {
        isFelow_count(page) <= 2 => [] | {
```
```
ext findParses(parPar: Par, par: Rent) -> Maybe<Paring> = {
    par.par.location + par.warnings + par.warnings + par.warnings
```

**What all three have learned is shape, not meaning.** `contract` blocks with method
signatures, `test "…" { expect … }`, `ext` declarations with type annotations,
generics, the `// --- Section ---` divider convention — a Stitch programmer would
recognise every construct. Identifiers are plausibly-shaped nonsense (`Handar`,
`Peries`, `spops`), prefixes stutter (`calcCalcProration`), and the comment English
is grammatical word salad. That last part is the 46%-comment-tokens finding visible
in the output: half the capacity is imitating English this rung cannot model.

Failures are one of three kinds — an expression collapsing mid-line, degenerate
repetition (`par.warnings + par.warnings + par.warnings`), or simply **running out of
the 96-token budget** mid-declaration, which scores as a parse failure but isn't one.
Parse rate understates coherence accordingly.

### The eyeball test gets the ranking wrong

By eye the ordering is D > all-30k > B: arm B produces `ALELONDOMALDOLDOW` and word
salad while the older, *worse* checkpoint looks tidier. By held-out NLL the ordering
is D > B > all-30k, and B is 0.13 nats ahead of all-30k.

So visual impression is not tracking model quality here — it is tracking which seeds
happened to fall over. The parse rates agree that nothing is resolved: 15% / 25% /
30% at n=20 is ±10pp, so D-over-B is ~1.1σ. D really is the best checkpoint, but
that rests on thirteen paired runs of held-out loss, not on these samples. Against
`drivel-all-30k` (17% perplexity gap) a visible improvement is plausible; against
arm B (2.8%) it is not, and reading one into three samples is the same error as
trusting a control that cannot discriminate.

## Caveats

- **The held-out-exemplar confound is measured, not argued away** — G/H put it at
  ~0.0035 nats of the 0.0216, and the deconfounded effect is +0.0181. But that pair
  is the noisiest here (paired se ≈ 0.010, so ~1.9σ alone). The claim rests on five
  same-sign measurements across two seeds, two held-out sets and three replacement
  slices, not on any one of them.
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
- ~~**`cargo xtask test` has one pre-existing failure on a clean tree**
  (`mutant_plan_tests::the_derived_plan_matches_the_previously_hardcoded_set`) —
  unrelated to this work, predates it.~~ **Retracted 2026-08-06:** verified on a clean
  tree, `cargo nextest run -p xtask` is **48/48 green**, that test included. The failure
  was real once, was fixed on 2026-07-29, and
  [loose-ends-2026-07-29.md](loose-ends-2026-07-29.md) already retired the claim — it
  then came back in this note a week later. Post 79's transcription hop, caught in the
  act: a stale caveat is cheap to copy forward and nothing type-checks a note.

## Task 3 — drivel was full, and "corpus first, not rung" is now wrong

Task 1 says marginal corpus has stopped paying **for drivel**. Two readings, and
they give opposite advice:

- **Capacity-bound.** drivel at 1.05M params has extracted what it can; a bigger rung
  would keep converting data into generalisation. → scale the rung, no generation
  hours needed.
- **Diversity-bound.** batch11 is the *same* 500-domain recipe sheet, so marginal
  tokens are increasingly redundant and no model size fixes it. → diversify the
  generator, which costs the scarcest resource this project has.

The curves lean toward capacity-bound. As corpus grows the train/held-out gap nearly
halves while held-out barely moves:

| arm | tokens | epochs | train (smoothed) | held-out | gap |
|---|---|---|---|---|---|
| B | 4.32M | 14.2 | 2.0736 | 2.5584 | **0.485** |
| E | 4.94M | 12.4 | 2.2212 | 2.5639 | 0.343 |
| D | 6.72M | 9.1 | 2.2471 | 2.5309 | **0.284** |

Training loss *rises* 0.17 as data is added and the model memorises much less — the
data is behaving like data — but held-out will not follow. That is what a capacity
ceiling near 2.53 looks like. (Partly confounded with the epoch count falling, so
suggestive rather than settled.)

The prior agrees: at 6.72M tokens `quip` (3.05M params) sits at **2.2 tokens per
parameter** against Chinchilla's 20, ~9× starved, where drivel at 6.4 is ~3× starved.
And the standing "corpus first, not rung" advice comes from
[batch9](batch9-findings.md#quip-3-the-parameters-buys-003-nats), where quip bought
0.030 nats — **measured at 2.93M tokens, a corpus that has since grown 2.3×.** That
answer is stale.

So: `quip` (3 049 920 params, d 192 × 6 layers × 6 heads) on the **same two corpora**,
two seeds each, 30k steps, same frozen vocab and held-out, same default 3e-3 LR.
Token counts are identical to the drivel arms — 4 318 391 and 6 724 381 — so this is
a difference-in-differences over the same token streams.

| rung | corpus | tokens | epochs | seed 0 | seed 1 | mean |
|---|---|---|---|---|---|---|
| drivel | B | 4 318 391 | 14.23 | 2.5584 | 2.5627 | 2.5606 |
| drivel | D | 6 724 381 | 9.14 | 2.5309 | 2.5398 | 2.5354 |
| **quip** | B | 4 318 391 | 14.23 | 2.4069 | 2.3906 | **2.3988** |
| **quip** | D | 6 724 381 | 9.14 | 2.3819 | 2.3244 | **2.3532** |

| step | drivel B | drivel D | quip B | quip D |
|---|---|---|---|---|
| 3 001 | 3.1702 | 3.2217 | 3.1321 | 2.9890 |
| 9 001 | 2.7885 | 2.7855 | 2.6362 | 2.6418 |
| 15 001 | 2.6781 | 2.6482 | 2.5119 | 2.5005 |
| 21 001 | 2.5972 | 2.5782 | 2.4392 | 2.4288 |
| 30 000 | 2.5584 | 2.5309 | 2.4069 | **2.3819** |

### The rung is worth 6× what the corpus is, per unit of effort

**quip − drivel is −0.162 nats on the B corpus and −0.182 on D**, at both seeds, against
seed spreads of 0.004–0.058. That is 3–10σ, and it is **5–6× the 0.030 nats batch9
measured at 2.93M tokens.**

| lever | cost | buys |
|---|---|---|
| +55.7% corpus (batch10-full + batch11) | **14.9 h of generation** | 0.025 nats |
| 3× parameters (drivel → quip) | **1.8× training wall-clock** | **0.162 nats** |

Roughly six times the return, and it spends none of the resource that is actually
scarce. **[batch9](batch9-findings.md#quip-3-the-parameters-buys-003-nats)'s "scaling
the rung is not what this ladder needs next; corpus is" was correct when measured and
is now wrong.** Two things changed: the corpus grew 2.3×, and that note's comparison
gave quip 20 000 steps against drivel's 30 000 — so its 0.030 was itself understated.

It is also a **lower bound**. drivel at 30k is near-converged (0.004 over its last 3k
steps); quip is still falling steeply (0.043 over its last 6k). More steps widen it.

### The difference-in-differences did *not* resolve

The question this was built to answer — does quip convert *marginal* corpus better
than drivel? — is not settled:

| | seed 0 | seed 1 | mean |
|---|---|---|---|
| drivel D − B | −0.0275 | −0.0229 | −0.0252 |
| quip D − B | −0.0250 | −0.0662 | −0.0456 |
| **DiD** | | | **−0.0204** |

quip's seed spread on that delta is 0.0412, giving a paired se of ≈0.021 — so the DiD
is ~1σ. The direction favours capacity-bound; the magnitude is not claimable.

**quip is markedly noisier than drivel** — seed spreads of 0.0163 (B) and 0.0575 (D)
against drivel's 0.0043–0.0156. The curves are smooth and monotone and the seeds
diverge progressively (0.037 apart at step 9001, 0.058 at 30 000), so this is genuine
seed variation, not a glitch. Most likely an endpoint sitting on a steep un-converged
curve. That noise is what swallowed the DiD, and a longer schedule would probably
shrink it.

So the honest synthesis is that the original either/or was a false dichotomy:
**drivel was capacity-bound on the corpus it already had** — hence quip's large
absolute win at *both* corpus sizes — while whether the *marginal* batch11 tokens are
redundant remains open. Nothing here argues against diversifying the recipe sheet;
it just is not the cheapest next move.

### Caveats specific to Task 3

- **The LR is untuned for quip.** 3e-3 is drivel's default and `cram/src/run.rs` does
  not scale it by rung, so batch9's quip used it too — which keeps this comparable to
  that datapoint but means quip may be mistuned. Both quip arms share it, so the DiD
  is internally valid; quip's *absolute* standing could move either way under a sweep.
  Given quip wins by 0.16 with a possibly-wrong LR, the direction is safe.
- **Equal steps is equal tokens seen, not equal compute.** quip burns ~1.8× the
  wall-clock per run (measured: ~7 000 tok/s against drivel's ~12–14k at the same
  4-way contention — better than the 3× its parameter count implies, because the
  wider matmuls use the AMX units more efficiently).
- **Neither quip arm has converged**, and quip@D is the least converged of all eight
  runs in this comparison.

### quip's output is not visibly better, and parse rate cannot see the gap

100 unconstrained 96-token samples each, same tiny eval root, best drivel against best
quip:

| checkpoint | held-out | as sampled | complete items |
|---|---|---|---|
| `drivel-D-b9b10b11` | 2.5309 | **34%** | 35% |
| `quip-D-b9b10b11-s1` | 2.3244 | 27% | **38%** |

The two parse measures **disagree on the winner**, and both gaps are ~1σ or less at
n=100 (se ≈ 4.5pp). So 0.207 nats — 23% lower perplexity, six times what the whole of
batch11 bought — produces **no resolvable difference in parse rate**, and no obvious
difference by eye either. quip's samples read marginally more fluently
(`computeAnglishment`, `trigaction` against drivel's `Peries`, `Handar`) and its code
blocks are structurally cleaner, but the character is identical: correct shape,
nonsense semantics. Nothing crossed a threshold.

quip's seed-0 sample, scored as *not* parsing:

```
test "boundMatch returns empty list for empty" {
    expect findMatchMatch(rem) == Some(0)
}

test "boundMatch returns negative for invalid range" {
    let result = concat(0, 10, [15])
    expect count(result) == 0
}

test "collectPath finds a valid paths" {
    let pattern = [
        Path(id: 13),
```

Three well-formed test blocks, failing only because the 96-token window cut it off
mid-list. Its seed-2 *success* is comments only, which parses trivially — the metric
is softer than it looks in both directions.

**The as-sampled/complete-items split is itself a finding.** quip has *more* complete
items but *fewer* as-sampled parses, which is the signature of writing longer
constructs that overrun the 96-token budget. If so the sampling window is now too
short to evaluate quip fairly and this comparison is mildly biased against it. Raising
`--samples` will not fix that; raising the sample *length* would.

This is the third time in this study that eyeballing samples failed to track the gate
metric — see also the drivel three-way above, where the ranking came out backwards.

## Artifacts

Checkpoints and curves in `checkpoints/` (gitignored), seventeen runs.

drivel: `drivel-B-repro`, `drivel-B-repro-s1`, `drivel-E-b9b10full`,
`drivel-E-b9b10full-s1`, `drivel-F1-stride`, `drivel-F1-stride-s1`,
`drivel-F2-ratio`, `drivel-D-b9b10b11`, `drivel-D-b9b10b11-s1`,
`drivel-G-ex-noexheld`, `drivel-G-ex-noexheld-s1`, `drivel-H-swap-noexheld`,
`drivel-H-swap-noexheld-s1`.

quip: `quip-B-b9b10`, `quip-B-b9b10-s1`, `quip-D-b9b10b11`, `quip-D-b9b10b11-s1`.

**`quip-D-b9b10b11-s1` (2.3244) is the best checkpoint this project has produced**,
0.207 nats ahead of the best drivel. Like every checkpoint here it is *not* promoted —
and note that promoting a quip rung would change the embedded model's size, not just
its weights.

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
