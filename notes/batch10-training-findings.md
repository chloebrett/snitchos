# batch10: what 47% more corpus is worth

The question: batch10 was still generating, so does it help to train on it, and how
much of that help is *volume* rather than batch10 being better corpus?

Every run below is `drivel`, 30 000 steps, seed 0 unless stated, against the frozen
`corpora/kvetch-batch9.vocab` (2048) and the frozen 116-program `corpora/heldout`, so
every number is comparable to every other. `--eval-batch` was raised from the default
64 to **1024** (~132k of the 303k held-out tokens), because the batch9 notes warn the
64-window sample is too noisy to quote standalone.

## The corpus

batch10 was frozen mid-generation into `corpora/batch10-snap` at 712 of 1000
candidates. Training against a directory that is still growing is not reproducible,
and the manifest is rewritten under you.

| | batch9 | batch10-snap |
|---|---|---|
| programs | 973 | 712 |
| tokens (frozen 2048 vocab) | 3 131 032 | 1 387 041 |
| bytes/token | 3.34 | 3.14 |
| median bytes | 8 465 | 5 202 |
| p90 / max bytes | 18 189 / 197 370 | 11 999 / 18 299 |
| parse deaths | 45% | **15%** |
| degenerate files (Finding 3) | 45 | **5** |
| comment share | 46.6% | 46.6% |

batch10 is visibly the better corpus — the batch9 findings' three named pathologies
(long programs, parse deaths, degenerate repetition) are all substantially reduced,
and the 197 KB monster is gone. **Hold that thought; it turns out to matter far less
than it looks.**

**Leak check.** batch10 reuses batch9's domains, so it can produce a program on the
same brief as a held-out one. Over all 712 × 116 pairs, the highest line-level
Jaccard overlap is **0.048**, and no pair reaches 0.20. Nothing here is a near-copy;
the trainer's exact-content dedup was not load-bearing.

## The arms

Arm C is the control that makes this readable: batch9 is subsampled to 51.3% of its
bytes (`corpora/batch9-half`, 510 of 973 files, selected by a running-ratio stride so
whole recipes are not dropped) so that C's total token count matches A's. A and C
differ only in *what* the tokens are; B and C differ only in *how many*.

| arm | corpus | tokens | epochs | held-out @30k |
|---|---|---|---|---|
| **A** | real + batch9 | 2 931 385 | 20.96 | 2.6689 |
| **B** | real + batch9 + batch10-snap | 4 318 391 | 14.23 | **2.5584** |
| **C** | real + batch9-half + batch10-snap | 2 920 193 | 21.04 | 2.6601 |

B beats A at **every** eval point from step 3001 onward, so this is not an endpoint
fluke:

| step | A | B |
|---|---|---|
| 3 001 | 3.2727 | 3.1702 |
| 9 001 | 2.8953 | 2.7885 |
| 15 001 | 2.7731 | 2.6781 |
| 21 001 | 2.7147 | 2.5972 |
| 30 000 | 2.6689 | **2.5584** |

## The noise floor

A and C were re-run at seed 1. Note the seed also reshuffles the held-out eval
windows, so this is *total* spread — an upper bound, which is the conservative
direction.

| arm | seed 0 | seed 1 | spread |
|---|---|---|---|
| A | 2.6689 | 2.6851 | 0.0162 |
| C | 2.6601 | 2.6462 | 0.0139 |

Pooled σ ≈ **0.013 nats**. Any effect below ~0.03 is not resolved by a single pair of
runs.

## The finding: it is volume, not quality

| effect | measured | vs σ |
|---|---|---|
| total (B − A) | **−0.111** | ~8σ |
| volume (B − C) | **−0.102** | ~8σ |
| quality per token (C − A) | −0.024 (seed 0: −0.009, seed 1: −0.039) | ~2σ |

**Swapping 1.39M batch9 tokens for 1.39M batch10 tokens bought about a fifth of what
adding them on top did**, despite batch10 having a third the median program size, a
third the parse-death rate, and a ninth the degenerate files. The quality effect is
real-looking but only ~2σ at n=2 and should not be quoted as a point estimate; the
volume effect is unambiguous.

For scale: tripling the parameters (drivel → quip) bought 0.030 nats. **47% more
corpus bought 0.111 — nearly 4× what tripling the model bought, at zero inference
cost.**

This is the fourth confirmation of "volume beats purity" in this project, and the
first one that actually *isolates* the axis. The previous three — keeping parse
deaths, keeping comments, and the quip run — each changed volume *and* something
else, so none of them separated the two. Holding volume fixed makes most of the
effect disappear.

### What follows from it

**Corpus generation throughput is the binding constraint, and raising yield per
candidate is worth much less than raising candidates per hour.** batch10's recipe
sheet was designed to fix Findings 1–4 (shorter programs, repaired rewind splice,
spread shapes) and it demonstrably did fix them — parse deaths fell from 45% to 15%.
That improvement is worth ~0.02 nats. Simply having the extra 1.39M tokens is worth
~0.10.

Corollary: the 79 `long`-rejected and 105 parse-dead candidates in batch10 should
stay in the training corpus, consistent with the batch9 finding that dropping parse
deaths cost 0.37 nats.

## An aside: batches 1–8 make it worse

The first run of the day trained on real + batch1–**9** (3 061 790 tokens) and scored
**2.6908**, against arm A's 2.6689 on real + batch9 alone. Adding batch1–8's 132 047
tokens *hurt* by 0.022 nats — 1.7σ, so suggestive rather than established, but it is
the wrong sign for a volume effect and those batches are early hand-tuned
experiments of mixed provenance. They are excluded from every arm above.

## Caveats

- **B was only run at seed 0.** The B−C and B−A effects are ~8σ against the pooled
  spread measured on A and C, so this is not in doubt, but B has no error bar of its
  own.
- **The held-out set is batch9-flavoured** — 108 of its 116 programs are batch9, 8 are
  real. It contains no batch10 at all. So this measures "does adding batch10 help on
  batch9-like held-out data", which is the conservative framing: batch10 gets no
  home-advantage.
- **Neither A nor B has fully converged**, though both are flattening (last 3k steps
  move 0.003 and 0.004 respectively). B is the steeper of the two, so a longer run
  would more likely widen the gap than close it.
- **Parse rate agrees in direction but cannot carry the claim.** Over 200 unconstrained
  96-token samples against `corpora/heldout`:

  | arm | as sampled | complete items |
  |---|---|---|
  | A | 56/200 = 28.0% | 55/200 = 27.5% |
  | B | 63/200 = 31.5% | 71/200 = 35.5% |

  Same sign as the loss, which is a useful sanity check that B is not degenerate. But
  at n=200 and p≈0.3 the standard error is ~3.2pp, so the as-sampled gap is ~1σ. The
  batch9 notes already established this metric cannot resolve two checkpoints half a
  nat apart; it certainly cannot resolve 0.11. **Held-out NLL is the gate metric, and
  this is why.**
- batch10-snap is 712/1000 candidates. The finished batch is ~40% larger again, so
  these numbers are a lower bound on what the complete batch10 is worth.

## Artifacts

Checkpoints in `checkpoints/` (gitignored): `drivel-A-b9`, `drivel-B-b9b10`,
`drivel-C-matched`, `drivel-A-b9-s1`, `drivel-C-matched-s1`, and the batch1–8 side run
`drivel-side-b1to9-30k`. `corpora/batch10-snap` and `corpora/batch9-half` are the two
derived corpora; both are reproducible from the scripts described above.

**`drivel-B-b9b10` is the best drivel checkpoint this project has produced** (2.5584
against the previously-kept `drivel-all-30k`'s 2.688). It is not promoted to the
embedded checkpoint here — that is a deliberate call, since `drivel-all-30k` is
committed, itest-asserted byte-exact, and embedded in a booting guest.
