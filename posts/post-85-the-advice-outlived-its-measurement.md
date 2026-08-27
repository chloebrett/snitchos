# Post 85 — the advice outlived its measurement

- nineteen training runs, three questions, and one uncomfortable finding that was sitting in the repo the whole time: **the rule the entire corpus effort was built on was measured once, at a corpus size we left behind two batches ago, and nobody wrote an expiry date on it.**
- [post 75](post-75-it-was-the-volume.md) established "volume beats purity" and the batch10 note turned it into policy — *candidates per hour beats yield per candidate*. that was correct when it was measured. this session measured it again at 2.3× the corpus and it is now false, and the lever it was pointing away from is worth six times more.
- the through-line, and it is not the finding: **almost every prediction I made in this session was wrong, and the only reason that was cheap is that each one was cheap to check.** the interim reading I volunteered at 15k steps flipped by 27k. my explanation for a noise floor was refuted by the run I proposed to confirm it. two throughput estimates were wrong in both directions. the measurements were fine; the commentary needed the measurements.

## the knee

- the question was simple. batch10 finished to 1000 candidates and batch11 landed — 910 programs, 5.6 MB, ~+2.4M tokens against a 4.3M-token corpus. does volume keep paying?
- arms are `drivel`, 30k steps, frozen 2048 vocab, frozen 116-program held-out, `--eval-batch 1024`, two seeds each. the first thing I ran was the *old* arm B, unchanged, because the KV-cache work had landed in `kvetch-model` since those numbers were taken and a changed forward pass would have invalidated the comparison silently. it reproduced **bit-for-bit** — all 31 rows of the published curve, loss, smoothed loss, lr, gradient norm.
- then:

| arm | corpus | tokens | held-out (s0 / s1) |
|---|---|---|---|
| A *(batch10 note)* | real + b9 | 2.93M | 2.6689 / 2.6851 |
| B | + b10-snap | 4.32M | 2.5584 / 2.5627 |
| E | + b10-**full** | 4.94M | 2.5639 / 2.5682 |
| D | + b10-full + **b11** | 6.72M | **2.5309** / 2.5398 |

- **+55.7% more corpus bought 0.025 nats. +47% bought 0.111 one batch earlier.** normalising for the log does not rescue it — 0.286 nats per log-unit of corpus from A→B, 0.062 from B→D. a 4.6× collapse in marginal return.
- and it splits unevenly. finishing batch10 (+622k tokens) bought **nothing**: +0.0055, the wrong sign. all of the movement was batch11.
- the cost framing is the one that matters. batch11 took **14.9 hours of generation** for 0.031 nats. at that rate the next 0.03 is another day, and the one after is more.

## the number that made the small effects legible

- the batch10 note's noise floor is σ ≈ 0.013, and its rule was "anything under ~0.03 is not resolved by a single pair of runs". D − B is 0.025. by that rule I had nothing.
- except the comparisons are **paired**, and I had not been thinking of them that way. two arms at the same seed share weight init *and* — because the held-out windows are drawn from a salted seed — **the same held-out eval windows**. only the training token stream differs, so the seed-level noise cancels.
- the evidence it works is arm E against arm B:

| | seed 0 | seed 1 |
|---|---|---|
| E − B | **+0.0055** | **+0.0055** |

- the same delta to four decimal places, at two independent seeds, on a metric whose *marginal* spread across arms is 0.004–0.016. a paired delta is resolvable an order of magnitude below what an unpaired one needs.
- this is the methodological thing I would keep from the session. it cost nothing — the runs were already there — and it is the difference between "no effect resolved" and three findings.

## what the exemplars are worth, and the confound that nearly ate it

- `examples/stitch/` is 30 hand-polished gold programs, the deliverable of [stitch 18](stitch-18-a-binding-is-not-a-boundary.md), which ended on "what they're worth to an actual model is the next measurement". they sit inside every arm as part of `--real-root` and nobody had measured them.
- the naive ablation — drop them, read the delta — moves two variables, because it also removes ~195 KB of volume and volume is exactly what dominates. so: **replace** them with an equal token count of generated corpus, and the arms differ only in what the tokens *are*. E and its swap arm landed 699 tokens apart out of 4.94M — 0.014%.
- two slices, not one, because the sampler is a real design choice: the running-ratio sampler that built `batch9-half` keeps a byte budget and therefore systematically prefers *small* files. a stride sampler is size-unbiased. both were drawn, and they bracket the choice.
- three measurements, all the same sign: +0.0159, +0.0272, +0.0234. swapping the exemplars out costs ~0.022 nats — from **1.16% of the corpus**.
- then the confound I nearly published around. **six of the thirty exemplars are in the held-out set.** so the exemplar arm was getting in-distribution practice for 4.29% of what it was scored on, and the measurement could not tell "exemplars teach Stitch" from "exemplars teach exemplars". for the confound to explain the whole effect the model would have to be 0.51 nats better on those six programs.
- four more runs against a 110-program exemplar-free held-out set: **+0.0181**. 84% survives. the confound is worth about a sixth, not the lot.
- per token that is ~13× generated corpus — and I had quoted ~20× before deconfounding, which is exactly the direction a confound flatters. **the ceiling I published was inflated and the deconfounding run is the only reason I know by how much.**

## the doctrine flips

- [batch9](../notes/batch9-findings.md) measured `quip` (3M params) at 0.030 nats over drivel and concluded, reasonably: *scaling the rung is not what this ladder needs next; corpus is.* that sentence has steered every hour of generation since.
- it was measured at **2.93M tokens**. the corpus is now 6.72M. and that comparison gave quip 20 000 steps against drivel's 30 000, so its 0.030 was understated to begin with.
- re-run on the same two corpora, same everything:

| rung | corpus | mean held-out |
|---|---|---|
| drivel | B | 2.5606 |
| drivel | D | 2.5354 |
| **quip** | B | **2.3988** |
| **quip** | D | **2.3532** |

- quip was still falling 0.043 per 6k steps at 30k, so I ran it to 60k. it converges — 0.001 over the last 6k — at **2.2571**, the best checkpoint the project has produced. at convergence the rung gap is **0.258 nats**, larger than mid-descent.
- ranked by measurement, and this is the whole post in four rows:

| lever | cost | buys |
|---|---|---|
| 3× params | 1.8× compute | **0.258** |
| 2× steps | 2× compute | 0.076 *(now spent — quip is converged)* |
| +55.7% corpus | **14.9 h of generation** | 0.025 |

- **six times the return, and it spends none of the resource that is actually scarce.**
- the sting in the tail: the reasoning that ruled out the *next* rung — cliché needs ~200M tokens by Chinchilla and we have 6.7M — is the same reasoning that ruled out quip. quip sits at 2.2 tokens per parameter, ~9× "starved" by that argument, and won by 0.258 anyway. **the error is still live, one rung up.**

## four times the samples lied

- across the full range this project has produced — 2.688 → 2.257, a **0.43-nat / 35% perplexity improvement** — I compared generated samples four times, three of them at n=100. parse rate: 25%, 15%, 34%, 27%, 32%.
- **no trend.** the best checkpoint scores below the worst of the n=100 three. and reading the samples is worse than useless: on one three-way, the *eyeball* ranking came out exactly backwards against the loss, because arm B happened to draw seeds that fell over.
- quip@60k's best sample is the most syntactically sophisticated line any rung here has produced —

```
isSipVol(state: ElevatorState, categories: List<Board>, w: Window) -> Bool =
    any(steps, p -> p.base == h)
```

— a fully annotated signature with generics and a lambda body, and semantically empty: parameters unused, `steps` and `h` undefined. its other "success" is a block of comments, which parses trivially.
- [post 77](post-77-the-number-that-could-not-see-it.md) already established parse rate cannot resolve checkpoints half a nat apart. this is the stronger version: **it cannot resolve them across the entire history of the project.**
- two readings and they are cheaply distinguishable. either every rung is still pre-threshold and coherence lives further up the ladder — or held-out NLL, on text that is ~46% English comments a 3M model cannot possibly model, has drifted away from the thing the model is *for*. scoring the existing checkpoints on comment-stripped text tells you which, with no retraining.
- what neither reading excuses: **nothing in this project measures Tab completion.** free-generation parse rate is not the task. that gap is now the top of the plan.

## the mutant that no test could kill

- the session ended by building the scorer that comment-stripped experiment needs, and mutation testing found two survivors in code I had not touched.
- one was **dead code wearing a live face**: `train` passes `learning_rate` into the optimizer's initial config, and the loop calls `set_learning_rate` before every single step. the initial value is overwritten before first use. no test could ever have killed that mutant, because deleting the line changes nothing — the mutant was not reporting a missing test, it was reporting a redundant line. deleted it.
- the other **looked equivalent and was not**. `weight_decay` is passed the same way, and `AdamWConfig::default()` and `TrainingConfig::default()` both happen to be 0.01 — so dropping it changes nothing observable either. I had already told Chloe to file it as equivalent. then I read the defaults: it is a live parameter that simply nothing exercises, and `.cargo/mutants.toml` is full of *genuine* equivalences (disjoint bits, no-op resizes). filing it there would have been an allow-list-by-omission wearing an understanding-shaped label — the exact failure the lint-gate work already caught once in this repo. it got a test instead.
- the test needs `warmup_steps: 1`, because the default 100-step warmup pins the lr at zero for both steps of a tiny run and decoupled weight decay scales with the lr. the two runs really would have come out identical, for a reason with nothing to do with plumbing. **a test that passes for the wrong reason is the same bug as a guard that checks nothing**, which is [post 80](post-80-the-control-passed-twice.md)'s whole theme.

## what I learned

- **a measured conclusion has a shelf life, and it is set by the conditions it was measured under.** "corpus first, not rung" was right at 2.93M tokens and wrong at 6.72M, and nothing in the note said which. the fix is not to distrust old numbers — it is to record the conditions *beside* the advice, so the advice expires visibly when they change.
- **pairing is nearly free and buys an order of magnitude.** two arms at one seed cancel the noise that two seeds of one arm measure. I had the runs; I did not have the framing.
- **design the control before you need it, and expect it to move the number.** the exemplar figure went from ~20× to ~13× when the confound was removed. both numbers came from careful work; only one of them is true.
- **my commentary was consistently worse than my measurements.** the 15k interim call flipped. the seed-noise explanation was refuted by the very run I proposed as its confirmation — the spread shrank 29% where the diagnosis needed most of it, on a run that had demonstrably converged. two throughput estimates missed in opposite directions. none of it cost anything, because each was checked within the hour. that is the only reason to keep saying the prediction out loud.
- **a survivor is a question.** one told me a line was dead. one told me a parameter was untested and I nearly mislabelled it as understood.

## what's next

- `plans/kvetch-next-measurements.md` has the seven steps. the two that gate everything else: score the existing checkpoints on **comment-stripped** held-out text — did the 0.43 nats buy code or English? — and build a **completion-shaped** metric, because free generation is not the task and has now failed four times to see anything.
- then: cliché, because the argument against it is the one that was wrong about quip. an lr sweep, because quip's seed spread stayed 3–10× drivel's *after* converging and I no longer have an explanation. and Chloe's idea, which is the sharpest open question — mix foreign code in, since batch11 was the *same* 500-domain recipe sheet and "the corpus is saturated" and "those particular tokens were redundant" are still not separated.
- not promoted, deliberately: `drivel-b9b10-30k` stays the embedded checkpoint. a quip promotion is a **kernel-image budget** decision before it is a model one — 12.2 MB of weights against 4.2 MB, on an image that already hit `OutOfFrames` at 4.5 MB.

Findings, in full and with the caveats: [notes/batch11-training-findings.md](../notes/batch11-training-findings.md).
