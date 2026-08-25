# Grammar-aware speculative decoding

**Status:** 📐 **DESIGN — maths settled, cutoff pending measurement.** How the
continuation oracle makes decoding cheaper *before* any second model exists, and
what sets the cutoff. Companion to
[generative-ladder.md](generative-ladder.md), which owns the classical
quip→ballad speculative-decoding plan; this doc covers the two grammar-derived
mechanisms that sit underneath it.

Related: [llm-design.md](llm-design.md) (the oracle's four consumers, the VF2
performance envelope), [babble-design.md](babble-design.md) (the drafter),
[../plans/drivel.md](../plans/legacy/drivel.md) (where the measurement lands).

---

## Two mechanisms, not one

At every decode step, constrained decoding already asks the oracle for the legal
token set — that cost is *sunk*. Both mechanisms here are ways of spending that
answer twice.

Let `n = |legal|` at the current position.

1. **Forced tokens (`n = 1`): skip inference entirely.** The target model has no
   choice — whatever its logits say, the mask leaves one survivor. So emit it and
   move on: **zero forward passes**, not amortized ones. Already committed to in
   [generative-ladder.md](generative-ladder.md) ("singleton-mask positions skip
   both models entirely") and it is the same mechanism as stim's forced-token
   auto-insert in [llm-design.md](llm-design.md).
2. **babble-drafting (small `n`): draft free, verify batched.** babble proposes,
   the target verifies under standard speculative sampling.

These are **not the same mechanism at different `n`**. (1) needs no verification
pass at all; (2) is ordinary speculative decoding with an unusually cheap
drafter. Worth keeping distinct — (1) is strictly better and unconditionally
correct.

## Why babble can draft here, when it cannot draft in general

[generative-ladder.md](generative-ladder.md) states that babble cannot draft:
speculative decoding's win is draft-target *agreement*, and uniform-random agrees
with nothing. That is right across the full ~58-class set, where acceptance is a
couple of percent.

It stops being right once drafting is **gated on small `n`**, because acceptance
is bounded below by `1/n`. And the usual objection to a cheap drafter — that
running it costs more than it saves — does not apply, because **the oracle query
is already paid for by the mask**. Drafting adds a sampling draw over a set the
decoder computed anyway.

## The maths

Speculative sampling accepts a draft token `t ~ q` with probability
`min(1, p(t)/q(t))`, where `p` is the target distribution. Expected acceptance:

```
α = Σ_t q(t)·min(1, p(t)/q(t))
  = Σ_t min(q(t), p(t))
  = 1 − TV(p, q)
```

Acceptance is exactly the **overlap** between drafter and target — one minus
their total-variation distance. With babble drafting uniformly over the legal
set (`q = 1/n`):

```
α = Σ_t min(1/n, p(t))          α ≥ 1/n
```

The lower bound is tight when the target is a point mass, and `α = 1` when the
target is itself uniform over the legal set. **So `n` does not determine `α` — it
lower-bounds it.** That bound is the only thing gating on `n` buys.

### Break-even

With draft length `k`, expected tokens per target forward pass is
`(1 − α^(k+1))/(1 − α)`; at `k = 1` that is `1 + α`. Let `c` be the cost of
verifying `k+1` positions relative to a single-token pass. Drafting wins iff

```
1 + α > c        ⟺        α > c − 1
```

Combining with the worst-case bound gives the cutoff:

```
n_max = ⌊ 1 / (c − 1) ⌋
```

**The cutoff is set by hardware, not intuition.** `c` is a measurement of how
much a batched verify costs relative to a single decode step:

| Regime | `c` (k=1) | break-even `α` | `n_max` |
|---|---|---|---|
| Purely bandwidth-bound (weights read once, FLOPs free) | ≈1.0 | ≈0 | unbounded — always draft |
| **VF2 at ballad** — simultaneously BW- and compute-bound per [llm-design.md](llm-design.md) | ≈1.5 | 0.5 | **2** |
| Purely compute-bound | ≈2.0 | 1.0 | never — speculation cannot help |

The 30M row of llm-design's envelope table is precisely why the board is not in
the free-lunch regime: at ballad we pay real FLOPs per extra verified position,
so `c` is not ~1. The honest expectation on-target is `n ≤ 2`, not the `n ≤ 3`
this started as.

Gating on `n` is only the cheap proxy. The sharper form is to gate on expected
value directly: measure the `n`-histogram, measure `c` on hardware, and pick the
policy maximizing `E[(1 + α(n)) / c]`.

### The bias tables are a speculative-decoding optimization

`α = 1 − TV(p, q)` improves as `q` moves toward `p`, and babble's draft need not
be uniform — it has bias tables. So the **shape-statistics pipeline that fills
those tables from measured corpus statistics also raises the acceptance rate**,
for free, without touching the decoder.

That pipeline was designed for Tier-0 corpus realism
([llm-design.md](llm-design.md)). Getting a decode speedup out of it is unearned,
and it means the tables have two consumers pulling in the same direction: a
drafter closer to real code is *both* a better corpus generator and a better
speculator.

## The measurement that sizes all of this

One number decides everything above and is computable today, with no model: the
**distribution of `n` over token positions**.

- `P(n = 1)` bounds the forced-token win — those steps are free.
- The histogram over small `n`, with `c`, sets `n_max` and the expected speedup.

**Measure it on both corpora, and expect them to differ.** The histogram over
babble output is the null model's trajectory through the grammar — where
uniform-random sampling happens to wander. Real code visits different states
entirely: deeper nesting, inside call arguments, after `match`. For sizing
speculation, what matters is the histogram along the *trained model's*
trajectory, and real Stitch is the far better proxy for that.

The babble-vs-real delta is not noise to be averaged away. It is the same
"distribution-vs-real delta" the per-batch report in
[generative-ladder.md](generative-ladder.md) already asks for, arriving early and
for free.

## Measured (2026-07-25)

`cargo run --release -p cram-corpus --bin legal-histogram`. 2000 babble programs
(54,683 decisions) against all hand-written Stitch in the repo (6 files, 7,676
decisions).

| `n` | babble | real Stitch | ratio |
|---|---|---|---|
| `= 1` (forced) | 19.9% | **8.3%** | 2.4× |
| `≤ 2` | 35.5% | 13.3% | 2.7× |
| `≤ 3` | 50.8% | 19.1% | 2.7× |
| `≤ 5` | 60.9% | 23.9% | 2.5× |

**Measuring on babble would have overstated the win by ~2.5×.** The caveat that
prompted this measurement was not a hedge — it was the finding. babble's walk
concentrates in low-branching states; real code lives in the wide ones (real
Stitch spikes hard at `n` = 17–18 and 25–27, the mid-expression positions where
two dozen classes are legal).

### What this does to the two mechanisms, on the board

- **Forced tokens: keep, unconditionally.** 8.3% of decode steps cost *zero*
  forward passes. Not spectacular, but free, exact, and it needs no second model.
- **babble-drafting at ballad on the VF2: not worth it.** With `c ≈ 1.5` the
  cutoff is `n_max = 2`, and `n = 1` is already handled by forced tokens — so
  drafting applies only to the `n = 2` slice, which is **5.0%** of decisions. At
  `n = 2` the worst-case `α = 0.5` is *exactly* the break-even `c − 1`. Guaranteed
  gain: zero. Actual gain depends on how far the target is from a point mass,
  and it is bounded by 5% of steps regardless.

That is a real negative result and it should stay written down: the mechanism is
sound, the maths is right, and the *distribution* is what kills it.

**Where it does pay: anywhere closer to bandwidth-bound.** As `c → 1` the cutoff
opens up and `n ≤ 5` covers 23.9% of decisions at `α ≥ 0.2`. That is the host
tier, the browser tier, and plausibly the smaller rungs (cliché and below) on the
board, where FLOPs stop competing with bandwidth. So the mechanism is a
**small-model / fast-memory optimization**, not a ballad-on-VF2 one — the
opposite of where it was first proposed.

### Caveats on the real-Stitch column

Six files and 7,676 decisions, dominated by `fs-image/stim/stim.st`, with
`prelude.st` contributing library-shaped code. This is the same genre skew the
canon stratum in [generative-ladder.md](generative-ladder.md) exists to fix.
Re-measure when canon lands; the ratio, not the absolute number, is the durable
finding.

## Open questions

- Does `c` on the VF2 actually land near 1.5, and does it move between cliché and
  ballad? **`c` need not be a microbenchmark** — it falls out of the runner's own
  telemetry, since `kvetch.prefill_tokens` vs `decode_tokens`, `tokens_per_sec`
  and the KV block counters in [llm-design.md](llm-design.md) already
  distinguish a bandwidth-bound step from a compute-bound one. The OS that
  snitches on itself can report which regime its own model is in, and the cutoff
  becomes a derived quantity rather than a tuned constant. Both mechanisms here
  are plug-and-play behind the mask, so this can be measured on real hardware
  whenever ballad lands rather than being decided now.
- Draft length `k > 1` under a grammar mask: each drafted token narrows the next
  legal set, so `n` is not independent across a draft window — does the chain
  concentrate on small `n` or escape it?
- Do forced tokens cluster (closing delimiters in a run) in a way that makes the
  win lumpy rather than uniform, and does that interact with KV-block
  granularity?
