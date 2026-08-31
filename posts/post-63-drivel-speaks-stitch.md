# Post 63 — drivel speaks Stitch

- rung 0 of the model ladder is **babble**: a sampler with no weights at all. it asks the parser what tokens are legal here and picks one. its output is valid Stitch by construction and means nothing — that's the point, it's the zero line every trained model gets measured against.
- rung 1 is **drivel**, ~1M parameters. the question for the day was the dumbest possible version of the whole arc: *can one million parameters beat zero?*
- the answer is yes, and almost nothing I predicted about **how** turned out to be right.
- [the previous post](stitch-16-the-ast-printer-and-ten-round-trip-bugs.md) built the pretty-printer that makes the corpus this model eats — babble renders a flat space-separated token stream, and training on that teaches a model babble's *renderer* rather than the language. it ends with "what that's worth to an actual model is the next post." this is that post.

```
contract buffer { }
sum entry<price> = field(@) | depth
let price = not @ or ().token
ext sum count<total> = price
on task -> @ -> @ { }
```

- that's drivel. 918,656 parameters, 35 minutes of training, no grammar mask — the model on its own. **91% of samples parse.**

## no framework, and one seam that matters

- I wrote the whole thing: BPE tokenizer, transformer forward pass, the backward pass by hand, AdamW, checkpoint format, training loop. ~1000 lines, `llm.c`-shaped. the value here is understanding, not delivery.
- the thing that keeps that choice reversible isn't a framework abstraction. it's **one trait with one method**:

```rust
pub trait Gemm {
    fn sgemm(&self, spec: GemmSpec, a: &[f32], b: &[f32], c: &mut [f32]);
}
```

- over 95% of training FLOPs are matmul, and the backward pass is *also* matmuls (`dX = dY·Wᵀ`, `dW = Xᵀ·dY`). norms, softmax, activations are memory-bound and cheap. so one function carries the entire performance story, and everything else stays plain readable Rust.
- three implementations, measured at the ladder's own projection shapes, GFLOP/s:

| backend | drivel 128×128 | ballad 384×1536 |
|---|---:|---:|
| naive (three loops, `no_std`, zero deps) | 3 | 2 |
| blocked + threaded | 131 | 135 |
| Accelerate (Apple AMX) | 1064 | 2500 |

- **the seam is worth ~500–1000×.** same model code, 2 GFLOP/s on the reference and 2.5 TFLOP/s on the matrix coprocessor. the reference stays legible *because* it isn't where performance lives. and going through Accelerate rather than hand-written NEON means AMX→SME and every future matrix unit lands for free — 20 lines of FFI that get faster when you buy a new laptop.

## the backward pass is checked against arithmetic, not against another program

- with no framework there's no second implementation to agree with. so correctness comes from the definition: **every backward op is checked against a finite-difference estimate of the same quantity**, per-op and then whole-model — all 1160 weights of a test config against a numerical derivative of the real loss.
- that's *stronger* than agreeing with PyTorch. two implementations can share a misconception; finite differences can't share one with you.
- two terms in there are the ones a hand-derivation drops: RMSNorm's `1/rms` coupling, and attention's softmax row-coupling `−Σ p·dp`. I claimed in a comment that dropping either gives a gradient that is "plausible, stable, and wrong" — then deleted each one to check. both fail the gradient check (RMSNorm at 40% error). the claim is now verified instead of asserted, and the tests demonstrably earn their place.
- the subtle thing about both: the wrong gradient still *trains*. slowly, to the wrong place. nothing except finite differences would tell you.

## the report caught what the estimate missed

- I built the training loop to report on itself — loss, smoothed loss, learning rate, gradient norm, tok/s, elapsed, ETA — because a training run's failure modes are quiet. it earned that on the first run: **~3000 tok/s**, extrapolating to ~9 hours against my estimate of 15–25 minutes.

| change | tok/s |
|---|---|
| first run | 3,000 |
| removed a `.to_vec()` and a loop-invariant from attention's inner loop | 4,136 |
| ran the batch's sequences concurrently | 26,063 |
| `Q·Kᵀ` and `P·V` as GEMMs instead of scalar loops | 37,055 |
| precomputed the RoPE rotation table | **55,945** |

- the loss trajectory was byte-identical through all of it, so each step is verifiably behaviour-preserving rather than "looks about right".
- **the biggest single win was a lookup table, not a matmul.** `rope_angle` called `powf` per (position, head, pair) in both directions — but the frequency depends only on the pair, and the rotation only on `(position, pair)`, never on the head. ~2M transcendental calls per step became ~4K. it beat every matrix optimization in the list, and it's the last place I looked.
- attention-as-GEMM under-delivered badly: 1.4× where the arithmetic said 3×. thread parallelism was *already* absorbing that cost, so the two overlap instead of compounding. second optimization of the same bottleneck pays much less than the first.

## 0%, and the model was fine

- first full run: 52,000 steps, 35 minutes, loss 6.93 → 1.93. unconstrained parse rate: **0 out of 200.**
- the eval printed three samples next to the number, and the bug was visible in seconds:

```
prod line()
contract span<port, buffer, buffer> { }
---
byte() = { }
```

- that's good Stitch, interrupted by `---`. **the corpus separator.** I tokenized the corpus *file* instead of the programs inside it, so `\n\x1e---\n` was in the training stream and the model dutifully learned to emit it. the vocab was built from parsed programs; the token stream wasn't. two paths that should have shared one — and the duplication lived in a binary where neither could be tested.
- the separator was **15% of the corpus** — 26.75M tokens down to 22.75M once it was gone. a sixth of drivel's compute went into learning a delimiter. (that's with drivel's own 1024-entry probe vocab; a token count means nothing without saying which vocab produced it, which is half of why the ladder's vocab freeze is a law rather than a preference.)
- a bare `0.0%` would have sent me through the backward pass, which was correct all along. printing samples beside the number is the cheapest debugging tool in the whole stack, and it's now permanent.

## loss went up and the model got better

- run 1 reached loss 1.93 and scored 0%. run 2 reached 2.26 and scores 91%.
- the separator was cheap-to-predict filler dragging the average down. **loss is not comparable across corpora**, only within one. ranked on loss alone, the broken run wins.
- that's a good argument for the ladder's eval gates being defined on held-out task metrics rather than loss, which they are — but it's the kind of thing you believe properly only after watching the number lie to you.

## the histogram that would have lied

- separate thread, same lesson. I worked out the maths for two decode tricks the parser makes possible: skip inference entirely when only one token is legal, and let babble draft when only two or three are. acceptance works out to `α = 1 − TV(p, q)`, break-even to `n_max = ⌊1/(c−1)⌋`.
- all of it is sized by one number: how often is the legal set small? measurable today, no model needed.

| `n` | babble | real Stitch |
|---|---|---|
| `= 1` (forced) | 19.9% | **8.3%** |
| `≤ 3` | 50.8% | 19.1% |

- **measuring on babble would have overstated the win by ~2.5×.** babble's walk concentrates in low-branching states; real code lives in the wide ones. the caveat was the finding.

## what I'm not pretending

- 91% is on *complete items*; as-sampled it's 85%, and the 6-point gap is a fixed 96-token budget stopping samples mid-construct. I report both because quoting only the higher one is flattering the result.
- this proves drivel learned the **grammar**, not that it learned anything about Stitch as written. it was trained on babble output — legal, meaningless, and generated by a walk over the parser. that was the point: a ceiling probe with data scarcity held out of the picture, so a failure would mean "1M params can't learn the grammar" rather than "not enough corpus". the real test is drivel against babble on held-out *human* Stitch, and that needs a corpus that doesn't exist yet.
- there's no KV cache, so sampling is O(T²). the vocab is 1024 entries trained on babble's lexicon and is explicitly disposable — the frozen ladder vocab has to come from real Stitch. 52,000 steps turned out to be 4.68 epochs rather than 4.0 once the corpus shrank, slightly past the knee.
- and the corpus moved under me while I trained. the previous post's printer got a fix touching about one program in 246,000; my run had already read the older bytes, and the cache digest can't see a change that small — which is exactly the caveat that post wrote down. the difference measured out at **951 tokens in 22.75M**, 0.004%, far below what a 200-sample parse rate can resolve. it doesn't move the result and it would absolutely move a tighter one, so it goes here rather than nowhere.
- and the honest through-line: my estimates were wrong in both directions, repeatedly. "20 hours at cliché" (wrong backend). "~1 TFLOP/s" (pessimistic by 2.5×). "5 minutes to implement, 3× faster" (took an hour, gave 1.4×, and the real win came from somewhere else entirely). **the benchmark, the gradient check and the sample dump were worth more than the reasoning every single time.** which is, I suppose, the same claim this OS makes about itself: don't argue about what it's doing, make it tell you.
