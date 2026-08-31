# Post 77 — training loss is blind to memorisation: held-out loss

- [post 75](post-75-corpus-volume-beats-corpus-quality.md) is the finding: more corpus beats better corpus, ~8σ, and the recipe-sheet improvements everyone had been optimising were worth a fifth of simply having more tokens. this post is the day before that — the instruments that finding needed, none of which existed, and a metric artifact I found while building them that I think matters more than the headline.
- the through-line: **for most of this project the training loop reported a number that was correct every step and could not see the only failure mode that was going to happen.**

## the loop was reporting the wrong loss

- `cram::run::train` reported loss, smoothed loss, learning rate, gradient norm, tok/s, ETA. every one of those is real. every one of them is **training** loss.
- at drivel's size that is not merely incomplete, it is blind in the exact direction the run is going to fail. a 1M-parameter model on a 1.5M-token corpus sees twenty-odd epochs. memorising is not the tail risk; it is the expected outcome. and a training loss that is falling looks *identical* whether the model is learning Stitch or learning this particular corpus by heart.
- so `train()` grew a second argument — a held-out token slice — and `Progress` grew a `held_out_loss` column. the change is small. what it exposed was not:

| steps | tokens seen | epochs | train loss | held-out |
|---|---|---|---|---|
| 3 000 | 6.1M | 5.5 | 2.772 | 3.326 |
| 10 000 | 20.5M | 18.3 | 2.226 | 3.123 |
| 15 000 | 30.7M | 27.4 | 2.109 | **3.084** |
| 30 000 | 61.4M | 54.8 | 1.881 | 3.093 |

- the held-out curve **bottoms out around step 12–15k and turns.** the training loss keeps falling right through it, all the way to 1.881, cheerfully, for another fifteen thousand steps of pure memorisation. every number in that last column was unavailable a day earlier.
- one design decision inside it is the whole reason the column is readable: **the held-out batch is fixed for the entire run**, drawn once from a salted seed. re-drawing per evaluation would move the curve for two reasons at once — the model changing and the sample changing — which is precisely the ambiguity it exists to remove. it is the same instinct as [post 71](post-71-an-async-audio-ring-and-a-real-time-deadline.md)'s `active: bool`: when a signal is ambiguous, take the disambiguator as an explicit input rather than inferring it.
- and it costs almost nothing, because there is no forward-only path in `cram` and I deliberately didn't write one. the held-out pass calls the same `loss_and_gradient` the training step does and throws the gradient away. a second implementation of the forward pass that disagreed with the first would be a far worse bug than the handful of milliseconds this wastes every `eval_every` steps.

## the split, and the leak I nearly shipped

- a held-out set has two jobs and it is easy to build one that does the first and quietly fails the second.
- **it must represent both corpora.** 38 hand-written programs and 536 generated ones; holding out 20% of the concatenation puts almost no hand-written Stitch on the measuring side. so the split is taken **per source**.
- **it must not be a suffix.** batch files are numbered in generation order, which is *recipe* order — ten consecutive files share a domain. taking the last fifth holds out twenty whole recipes and measures generalisation to unseen *domains*, which is a different and much harder question than the one being asked. so it is a stride: every fifth file.
- then the leak. to compare "trained with comments" against "trained without" I needed both runs measured on the *same* comment-free held-out stream, which meant writing a stripped copy of the split to a second directory. the exclusion that keeps held-out programs out of training compared program text — and a stripped copy is not the same text. all 116 held-out programs would have walked straight back into training, with nothing downstream reporting it, and both runs would have produced a plausible number describing nothing.
- the fix is that the exclusion key is the program's **comment-stripped** form. the comments are not what is being held out; the program is.

## 47% of every token was English

- and then the thing I actually want on the record. nothing strips comments before training, and the generator prompt asks for `// Why:` commentary deliberately, so:

| corpus | comment bytes | comment tokens |
|---|---|---|
| batch9 | 44.2% | **47.3%** |
| real hand-written `.st` | 37.8% | 38.3% |

- **nearly half of every token a 1M-parameter model saw was English prose it has no hope of modelling.** the samples make it plain — `getAmnestyPolicyToBmnesty`, `puzzle.persudget(puzzle)`, "a rolling-recal days". that is capacity spent producing word-shaped noise, competing for the same parameters as the grammar.
- the obvious move is to strip them. I did, and it is worse: two 30k-step runs, same vocab, both scored on the *same comment-free* held-out stream, so this is a like-for-like measurement of predicting **code**:

| | training tokens | epochs | held-out |
|---|---|---|---|
| with comments | 2 932 977 | 21.0 | **2.4497** |
| stripped | 1 500 727 | 40.9 | 2.7192 |

- stripping halves the corpus and costs 0.27 nats. it is the volume finding again, arriving before post 75 managed to isolate it — and there is a third possibility this experiment cannot separate out, which is that the comments are *load-bearing*. `// Calculate the remaining free space in a bin` sitting directly above `freeSpace(b: Bin) -> Int` is lexical overlap blatant enough for even this model to use. distinguishing "volume" from "free hint" needs a comments-kept run padded back down to 1.5M tokens, which nobody has run.

### the part that isn't about comments at all

- while sampling from those checkpoints I checked something I had no reason to check, and it is the finding of the session:

- over 60 samples at 96 tokens — **5 of the 20 that parsed contained no code line at all.** a file of pure comments parses trivially. parsing samples averaged **67%** comment text; failing ones **37%**. parse rate among samples that actually contain code: 15/55 = **27%**, against a headline of 33%.

- so the metric this project has been quoting for the language model since [post 63](post-63-drivel-speaks-stitch.md) is measuring grammar *and comment-fraction together*, and the two move in the same direction. this is [post 73](post-73-cram-eval-the-baseline-nobody-measured.md)'s finding one floor down — that post found the baseline had never been measured; this one finds the generative metric is partly measuring the corpus's prose habit.
- it also explains a number I would otherwise have reported as a regression. the comment-free model scores 4.5% parse against the comment-trained model's 25.0% — but at a fixed 96-token budget a comment-free sample is *far more code*. it has ~96 tokens of syntax to keep balanced where the other has ~50. that is a harder exam, not a worse student, and `complete items` (22.0% vs 28.0%) shows the gap mostly closing once you stop counting comment padding as success.

- and the sharpest version, because it is about the instruments rather than the corpus: **parse rate could not see the difference held-out loss could.** two checkpoints, 23.5% and 25.0% at n=200 — inside sampling error, indistinguishable. the same pair, on held-out loss: **0.48 nats apart.** a coarse metric does not announce that it cannot tell. it reports a tie, and a tie reads as "no effect."

## the small ones

- **a token count is meaningless without naming its vocab**, and I confused myself with my own numbers for ten minutes. the identical 9 787 332 bytes of training text is **6 397 964** tokens under the babble-trained 1024 vocab and **2 933 322** under the batch9-trained 2048 one, at 1.53 vs 3.34 bytes/token. I reported the first, then the second, and the corpus looked like it had lost half its content. bytes are the vocab-independent anchor. this is exactly the trap [post 73](post-73-cram-eval-the-baseline-nobody-measured.md) names one level up — loss is not comparable across corpora — and it catches plain counts too.

- **choosing the vocab is choosing how much of the model is a lookup table.** 2048 entries, from a sweep of 512/1024/2048/4096/8192. above 2048 the embedding table starts to dominate a 1M-parameter rung — 8192 nearly doubles the model — while each row gets less evidence: 2048 gives ~550 occurrences per token, 8192 gives ~100. and the built-in longest-token report is the check that catches it qualitatively: at 2048 the tail is indentation runs and comment rules, which is what a *code* vocab should spend its tail on; at 4096 it already contains `" reduceReduceReduceReduceRedu"` — a degenerate-repetition artifact from one bad generated file, memorised as a token.

- **the `--name` flag exists because I destroyed a reference curve.** checkpoints were named `<rung>-<seed>`, so a 20-step smoke run overwrote `drivel-0.tsv` — the 52 000-step curve every timing in this post is calibrated against. I had already read the numbers out of it. it is a gitignored derived artifact. it is also gone, and a step sweep would have overwritten itself three more times before I noticed.

## what I learned

- **an instrument that cannot see the failure mode is not a partial measurement, it is a blind one.** training loss was correct every single step and could not distinguish the two outcomes that mattered. adding the held-out column did not improve a number; it created the only one that could answer the question.
- **when a signal is ambiguous, take the disambiguator as an input.** the fixed held-out batch, so consecutive points differ by the model alone. same move as the audio ring's `active` flag, in a completely different register.
- **a coarse metric reports a tie, not an uncertainty.** parse rate said two checkpoints were the same; they were half a nat apart. the failure mode of an underpowered measurement is that it looks like a *result*.
- **the metric and the corpus can share an assumption.** parse rate rewards comment-heavy output because a comment-only file parses, and the corpus is 47% comments. neither is wrong on its own. together they inflate the headline number of the whole language-model arc, and it took sampling sixty programs and *counting the code lines* to see it.
- **a copy is not the original, and content-keyed exclusion has to know that.** the held-out leak would have been silent, and both arms of the comparison would have reported confident numbers about nothing.

## what's next

- the corpus grew while I was writing this. batch11 landed — 910 programs, 13% parse deaths against batch9's 45%, with the length cap and the `abandoned` field from the batch9 findings both implemented — and batch10 completed to a thousand. that is roughly **2.4M tokens nobody has trained on**, against a 4.3M-token corpus. post 75's own number says 47% more corpus bought 0.111 nats; this is +55%, and it is twenty-five minutes of compute.
- and [stitch 18](stitch-18-thirty-example-programs.md) ends on a question this machinery is now built to answer and nobody has asked: thirty hand-polished exemplars sit in the corpus, and *what they are worth to an actual model* has never been measured. post 75's volume-beats-quality result compared two machine-generated batches of similar provenance. hand-polished is a different regime — and post 75's own aside, that batches 1–8 made things *worse*, says provenance has a floor somewhere. the exemplars are already inside every arm, so the measurement is an ablation: drop them, read the delta.
