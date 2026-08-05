# Post 75 — it was the volume

- post 66 was about manufacturing a corpus for a language with no users: 100+ recipes, a local model, a guard that rewinds the generation whenever it emits something Stitch can't parse. batch9 ran 23.5 hours and produced 973 programs.
- batch9's findings were a list of pathologies, and batch10's recipe sheet was designed to fix every one of them. it did. **parse deaths fell from 45% to 15%.** the median program shrank by a third. the 197 KB monster was gone.
- then I measured what that improvement was worth against simply having *more tokens*, and the answer reorganised my whole sense of where effort belongs.

## the thing you have to do before you can measure anything

- the trap in "is the new corpus better?" is that the new corpus is also **more corpus**, so any comparison moves two variables. this project had already concluded "volume beats purity" three times — keeping parse deaths, keeping comments, the quip run — and *every one of those* changed volume and something else at the same time. three confirmations of a thing none of them had actually isolated.
- so: three arms, and the third one is the whole experiment.

| arm | corpus |
|---|---|
| A | real + batch9 (3.13M tokens) |
| B | real + batch9 + batch10 (4.52M) |
| C | real + batch9 **subsampled to 1.39M**, matching batch10's token count |

- A and C differ only in *what* the tokens are. B and C differ only in *how many*. C is batch9 cut down by a running-ratio stride so whole recipes aren't dropped — the point is to keep the mixture and lose only the volume.
- everything is against the same frozen 2048 vocab and the same frozen 116-program held-out set, so every number compares to every other. `--eval-batch` went from 64 to 1024 because batch9's own notes warned the 64-window sample was too noisy to quote.

### and a noise floor, before any claim

- A and C re-run at seed 1. the seed also reshuffles the eval windows, so this is *total* spread — an upper bound, which is the conservative direction.

| arm | seed 0 | seed 1 | spread |
|---|---|---|---|
| A | 2.6689 | 2.6851 | 0.0162 |
| C | 2.6601 | 2.6462 | 0.0139 |

- pooled σ ≈ **0.013 nats**. anything under ~0.03 is not resolved by a single pair of runs. having this number *first* is what stops the next table from being a story about noise.

## the finding

| effect | measured | vs σ |
|---|---|---|
| total (B − A) | **−0.111** | ~8σ |
| volume (B − C) | **−0.102** | ~8σ |
| quality per token (C − A) | −0.024 | ~2σ |

- **swapping 1.39M batch9 tokens for 1.39M batch10 tokens bought about a fifth of what adding them on top did** — despite batch10 having a third the median program size, a third the parse-death rate, and a ninth the degenerate files.
- the quality effect looks real but it's ~2σ at n=2 and I'm not quoting it as a point estimate. the volume effect is not in doubt.
- for scale, and this is the number that actually changed my plans: tripling the parameters (drivel → quip) bought 0.030 nats. **47% more corpus bought 0.111 — nearly 4× what tripling the model bought, at zero inference cost.**

### what follows from it

- **corpus generation throughput is the binding constraint, and raising yield per candidate is worth much less than raising candidates per hour.** batch10's recipe sheet demonstrably fixed the things it targeted, and that improvement is worth ~0.02 nats. simply having the extra 1.39M tokens is worth ~0.10.
- which inverts where the work goes. I'd been treating the guard, the recipe design and the parse-death rate as the frontier. they're real, they're just *cheap* relative to wall-clock generation throughput.
- corollary I'd have got wrong on instinct: the 79 `long`-rejected and 105 parse-dead candidates in batch10 **stay in the training corpus**. dropping parse deaths cost 0.37 nats in the batch9 experiment. broken programs are still Stitch-shaped tokens, and the model is learning shape.

### an aside that points the other way

- the first run of the day trained on real + batch1–**9** and scored 2.6908, against arm A's 2.6689 on batch9 alone. adding batch1–8's 132k tokens **hurt** by 0.022 nats.
- 1.7σ, so suggestive rather than established — but it's the wrong *sign* for a volume effect, and those batches are early hand-tuned experiments of mixed provenance. excluded from every arm above, and worth watching: "volume beats purity" evidently has a floor somewhere below which provenance starts mattering again.

## the boring finding that saved the run

- batch9 crashed at 973 of 1000 candidates, 23.5 hours in.
- the only reason any of it survived is an **uncommitted** incremental `write_manifest` in `xtask-cram/src/generate.rs` — a change sitting dirty in the tree, which happened to be flushing verdicts as it went rather than at the end.
- a 23-hour job that writes its results once, at the end, is a 23-hour job with a single point of total loss. that's now committed, and it's the same lesson as the emulator swallowing its halt reason in post 74: **the failure mode of a long-running process is decided by what it has already written down.**
- the other batch9 finding worth keeping is Finding 1, because it's so cleanly monotone: parse-death rate by decile of program size runs 16% → 92%. length is the variable. batch10's whole recipe sheet came out of that one table, which is the best possible outcome for a findings note.
