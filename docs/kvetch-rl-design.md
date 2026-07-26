# Verifier-Filtered Training for the kvetch Ladder

**Status: design/exploration only. Nothing here is built.** The prerequisites section lists
what would have to exist first, and Increment 0 is a gate that can kill the rest
of the document for the cost of two `cram` runs.

This is the reinforcement-learning / reasoning-data branch of the model ladder.
Background: [llm-design.md](llm-design.md) (the arc, the rungs, the provenance
paper), [../plans/drivel.md](../plans/drivel.md) (rung 1, trained, 91% parse),
[language-design.md](language-design.md) (Stitch itself).

---

## 1. The governing law

> **RL does not create capability. It reallocates probability mass toward
> behaviours already present in the sample distribution that happen to work.**

Everything downstream follows from this sentence, so it goes first.

A policy-gradient update weights the ordinary cross-entropy gradient by an
*advantage* — "was this rollout better than expected?" It pushes up token
choices that appeared in successful samples and pushes down ones that appeared
in failures. It cannot push up a behaviour that never got sampled. Whatever the
model can already do occasionally, RL makes habitual and targeted; whatever it
cannot do at all, RL leaves untouched.

Two consequences that shape this whole design:

**The zero-advantage sandwich.** Under GRPO the advantage of rollout *i* in a
group of *G* is `(r_i − mean(r)) / std(r)`. If every sample in the group
succeeds, the mean is 1 and every advantage is 0. If every sample fails, the
mean is 0 and every advantage is 0. `std` collapses in both cases. **A group
teaches nothing unless it contains both successes and failures.** Filtering
zero-variance groups and shaping problem difficulty to keep pass rates in a
learnable band is not a refinement — it became standard practice across the
2025 RLVR literature precisely because this failure is the default.

This matters immediately: drivel sits at ~91% parse. Parse-reward is already
saturated — most groups would be all-success and contribute nothing.

**The scale evidence is discouraging, and it is not ambiguous.** DeepSeek ran
the relevant experiment inside the R1 paper: distilling R1's outputs into
smaller bases beat running RL directly on a 32B base, by a wide margin (~72% vs
~47% AIME for 32B). Their stated conclusion was that distillation is more
effective and economical at that scale, and that crossing the boundary needs
larger bases and more compute. Subsequent work reinforced this from another
angle — RLVR-trained models beat their base at pass@1 while the *base often
catches up at large pass@k*, i.e. RL sharpened sampling rather than expanding
the capability frontier. A related cautionary result: several small-model RLVR
gains replicated on math-heavy Qwen bases but not Llama ones, and in some cases
appeared even under spurious rewards — the gains were latent pretraining
behaviour being unlocked, not RL teaching anything.

The honest split:

| | ~1B | ~10B | ~30B+ |
|---|---|---|---|
| **Exhibit** long-chain reasoning, self-checking, backtracking | yes, via distillation | yes | yes |
| **Discover** them from outcome reward alone | narrow scoped tasks only | sharpening, not discovery | underperforms distillation |

The kvetch ladder tops out around 30M parameters. **Emergent reasoning is not
on the table and no plan here may depend on it.** What is on the table is
conformance and reliability — a rung that emits Stitch which typechecks, and
repairs Stitch that doesn't. That is a real result. State it as the goal so
nothing fails late and expensively for having quietly assumed otherwise.

---

## 2. What this project has that others don't

The usual bottleneck in RL-for-code is verification: you need a sandbox per
rollout, or a judge model that is noisy and inherits its own biases. That
bottleneck does not exist here. But the advantage is narrower and more specific
than "we have a checker," so it's worth enumerating precisely.

**Verification is free, exact, and graded.** `stitch/src/parser.rs`,
`check.rs`, and `interp.rs` are in-process Rust in the same workspace as the
model. A rollout is scored by a function call in microseconds, deterministically,
with no false positives. And it isn't one bit — it's a ladder (§3).

**Problems are free too, and this is the bigger asset.** Most RL-on-code work
is bottlenecked on *tasks*, not on checking; benchmark problems and real issues
are finite. `cram_corpus::generate(seed, count, layout)` is a generator. Corrupt
its output and every corruption is a task. Unlimited supply — **and at
difficulty we control**, which is the direct fix for the zero-advantage
sandwich. We do not have to hope a rung lands in the learnable band; we can
generate problems into it and keep them there as the model improves.

**Effects are reified.** Stitch has effect handlers running the full pipeline
(`ast.rs` → `lower.rs` → `core_ir.rs` → `interp.rs`, with `check.rs` aware of
them). A rollout therefore yields not just a value but an **effect trace**. That
permits rewarding *process*, not only outcome — see §6, which is the answer to
the most-likely reward-hacking failure.

**Execution is bounded.** `stitch::interp::eval_program_with_fuel` faults with
"evaluation fuel exhausted" rather than hanging. Any execution-based reward
needs this and it already exists.

**Where the advantage does *not* extend.** It is tempting to say "we can emit
every trace we'd want to train on, byte-exact." That conflates two artifacts:

- A **verifier** checks an answer. Free here. Real advantage.
- A **trace generator** produces the reasoning that *finds* the answer.

The typechecker cannot do the second. What a bidirectional checker emits while
walking an AST is a **derivation**: check this against that, synthesize here,
unify there. It is complete and correct and contains **no search** — no dead
ends, no abandoned hypothesis, no detection of error. It is the proof, not the
finding of the proof. Training on clean derivations teaches a model to *render*
derivations, which is a formatting skill, and teaches it that answers arrive
without search — close to the opposite of the target.

**Rule: never train on the checker's own derivations.** §5 is how to get search
structure anyway.

---

## 3. The oracle ladder

Five exact checkers at increasing strictness. Partial credit from real oracles
rather than from a judge model.

| Rung | Oracle | Cost | Notes |
|---|---|---|---|
| 0 | parses | µs | **Do not train on this.** See below. |
| 1 | typechecks | µs | The first real target. |
| 2 | runs under fuel without faulting | ms | Needs `eval_program_with_fuel`. |
| 3 | passes supplied tests | ms | Requires held-out tests — see §7. |
| 4 | its tests kill mutants | ms×N | Anti-hack reward; needs the mutation tester. |

**Parse-reward is the wrong target and should be skipped entirely.** Two
reasons. It is already saturated at drivel (§1). And grammar-constrained
decoding gives 100% parse *by construction, today* — the machinery was already
costed in the spec-decoding work. Spending an RL loop to move 91%→99% on a
property a decoder mask makes unviolatable is strictly dominated.

Which gives the boundary that justifies this whole document:

> **RL earns its keep only on properties a prefix mask cannot express.**

Whole-program type correctness. Tests that pass. Using the capability you were
granted. These are semantic, non-local, not prefix-checkable — a mask cannot
enforce them and a corpus fix will not teach them. Rung 1 is the sweet spot for
a first attempt: strictly harder than parsing, still microseconds, still exact,
and very likely to land in the learnable band at some rung of the ladder.

---

## 4. The curriculum dial

Because problems are generated, difficulty is a knob rather than a hope.

```
program  ──corrupt(k)──▶  broken program  ──▶  task: "make this typecheck"
```

`k` = number and severity of corruptions. Sweep it, measure group pass rate,
and hold the sampler where pass rates sit in roughly the 5–60% band — the
region where groups actually contain both outcomes. As the policy improves, `k`
rises to track it. Concretely:

- Discard zero-variance groups before the update (they contribute nothing and
  cost a backward pass).
- Log the realised pass-rate distribution per `k` every round. If the band
  drifts, the curriculum is stale; that is a metric, not a vibe.
- Corruption operators should come from the same engine as the Stitch mutation
  tester (designed, unbuilt) so there is one definition of "a small wrong
  change" in the workspace.

**Known caveat: the generator's distribution is a ceiling.** Verifier-filtered
training on synthesised problems converges to the support of the problem source.
A model that gets excellent at `cram-corpus` Stitch may not be good at Stitch.
Mitigations (mutate the generator itself, mix in hand-written programs, hold out
a distribution for eval) must be designed in from the start, not bolted on after
a good number appears.

---

## 5. Manufactured repair traces

This is the centrepiece and the one genuinely novel item in the document.

The scale evidence (§1) says small models need *distilled* traces rather than
discovered ones. We have no bigger Stitch model to distill from — no teacher, no
cold start. And §2 says the checker cannot supply search structure.

**So manufacture the search.** Cold-start data needs messy chains containing real
search — wrong turns, detection, repair. Construct exactly that:

1. Take a correct program (`cram_corpus::generate`).
2. Corrupt it. The corruption engine knows what it did.
3. Record the **repair path**: broken state → checker's complaint → fix →
   next complaint → fix → clean.

Run the corruption sequence backwards and you have a trace that begins wrong,
notices, and recovers. The wrong turns are constructible **because we made
them**. Ground truth is known at every step, so the trace is byte-exact and
labelled — and unlike a derivation it has the shape of *thinking* rather than
the shape of a proof.

This is not distillation from a teacher model. It is **distillation from a
constructed search**, available because this workspace owns the corruption
engine, the checker, and the interpreter at once. I am not aware of anyone doing
this, and it is the part of this design most likely to be a result rather than a
recipe application.

**Design notes and honest risks:**

- The repair order is *chosen by the constructor*, not discovered. A trace that
  always fixes errors left-to-right teaches left-to-right repair. Randomise
  repair order, and include traces that attempt a wrong fix first and revert —
  the revert is the behaviour worth teaching.
- The checker's diagnostics are already good (spanned faults, `file:line:col`,
  backtraces), so the "complaint" half of each step is real text the model will
  also see at inference. That is a genuine train/inference match, not a proxy.
- Risk: manufactured search may be *too clean* in a different way — real
  confusion doesn't look like reverse-corruption. This is a real threat to the
  idea and Increment 0 is designed to detect it.
- §9 describes a **second** trace source (guided search) whose wrong turns are
  genuinely explored rather than constructed. Its failure mode is the opposite
  of this one, which is a good reason to build both and compare.

---

## 6. Rewarding process, not only outcome

Outcome reward cannot distinguish performative self-correction from real
self-correction, because both produce the same final answer. This is visible in
shipped frontier models: reasoning models emit "Wait." reflexively, sometimes
followed by restating the same claim unchanged. The token pattern was reinforced
because it *correlated* with correct answers, and length pressure made it a cheap
way to spend tokens. The form got reinforced along with the substance.

Effect handlers let us do better. A rollout produces an **effect trace**, so
reward can ask questions an outcome scalar structurally cannot:

- Did it use the capability it was granted?
- Did it reach for authority it did not hold?
- Did it do the work, or short-circuit to something that merely returns the
  right value?

A reward hack is by definition *a wrong process with a right outcome*. An
observable process is therefore the natural defence, and almost nobody doing
code RL has one — they would need to instrument a language they don't own. This
is the same insight as the mutation-testing reward at rung 4: don't ask "did it
pass," ask "did it do the thing."

Do not read this as "outcome RL degrades the model" — it does not (§10). The
claim is narrower: outcome reward is *blind* to the difference between real and
performative process, so it reinforces whichever is cheaper to produce.

---

## 7. Reward-hacking register

Write the hacks down before running anything; each needs a countermeasure or an
explicit acceptance.

| Hack | Countermeasure |
|---|---|
| Model writes both code and tests → `test "x" { expect true }` | Score suites by **mutants killed**, not by passing. A vacuous suite kills zero. Or supply held-out tests and let the model write only the implementation (cleaner, but an easier and less interesting task). |
| Emit the same six-token program that typechecks, forever | KL penalty against a frozen reference; diversity term; held-out distribution in eval. |
| Infinite loop to avoid a wrong answer | `eval_program_with_fuel`. Already solved. |
| Performative self-correction ("Wait.") | Effect-trace reward (§6). Outcome reward cannot see this. |
| Overfit to `cram-corpus` distribution | §4 caveat — mixed and held-out distributions, designed in up front. |

---

## 8. Algorithm choice

Ordered by cost. **Start at the top and only descend if the previous rung
plateaus.**

1. **Rejection-sampling SFT (STaR / RFT / expert iteration).** Sample K per
   prompt, keep what the oracle accepts, fine-tune on survivors with the
   existing cross-entropy loop. Zero new trainer math — the new code is
   sampling and plumbing, not calculus. Gets the "push good up" half only; no
   negative signal. This is where to start.
2. **GRPO.** Sample a group of G per prompt, `A_i = (r_i − mean)/std` within the
   group, scale the existing CE gradient at the logits by `A`. `dL/dlogits`
   becomes `A · (p − onehot)` — one multiply. No critic, no value network. Adds
   real negative signal, which is most of the value.

**Not PPO.** It adds a learned critic (a second network of comparable size, its
own backward pass, memory, hyperparameters and failure modes), GAE (advantage
interpolation that earns its keep with dense rewards over long horizons — we
have one bit at the end of a short sequence), and ratio clipping (off-policy
correction for reusing a batch across several updates; with a single update per
batch the ratio is identically 1 and clipping is a no-op). Against a
hand-written backward pass verified by finite differences, that is a large,
specific cost for a payoff in a regime we are not in.

**Not PRMs.** Tried and reported as a failure during R1's development, for a
reason that generalises: **you cannot cheaply and reliably score an unfinished
thought.** A process reward model needs step-level labels that are expensive by
hand and noisy when automated, and being a *learned* reward it gets hacked and
needs periodic retraining.

Worth noting for our case specifically: the oracle ladder (§3) is a *cheap exact*
partial-credit signal — the thing PRMs were trying to approximate with a learned
model. That is unusual, and it is why the MCTS objection does **not** transfer
here (§9).

**MCTS over open-ended natural-language reasoning: no.** Branching over a ~100k
vocabulary, no natural node boundary in text, and the same missing value model
as PRMs. **MCTS over typed program synthesis: a live candidate**, for reasons
that are specific rather than optimistic — see §9. This is a revision of an
earlier draft of this document, which refused MCTS wholesale by importing
natural-language constraints into a problem that does not have them.

---

## 9. Search as a trace generator (typed MCTS)

MCTS failed for R1-style reasoning. **Typed program synthesis is not open-ended
reasoning**, and every requirement that failed there is satisfied here.

| MCTS needs | General CoT | Stitch synthesis |
|---|---|---|
| Small branching factor | ~100k vocabulary | **~16, measured** (below) |
| A meaningful node boundary | none — token? clause? | a completed AST node / item |
| A value estimate at interior nodes | needs a learned model (hackable) | **the type checker, exact, unlearned** |
| Cheap rollouts + reliable terminal signal | expensive, sparse | small model, short programs, fuel-bounded |

**The branching factor is already measured.** `cram_corpus::legal_counts` gives
the legal-continuation count at each position, and the `cram-eval` floor —
uniform-over-legal at 2.758 free-nll — implies a geometric mean of about
`e^2.758 ≈ 16` legal tokens per position on held-out Stitch. Under
grammar-constrained decoding the tree is *narrower than Go's* (~250). This is a
number in the repo, not an estimate.

**The type checker is a partial value function.** Bidirectional checking is
incremental by construction: a partially-built program can be checked as far as
it goes. That supplies a cheap, exact, **unlearned** signal at interior nodes —
precisely the piece whose absence sank both PRMs and MCTS at frontier scale, and
it cannot be reward-hacked because there is no model to hack. This is also the
classical result from outside LLMs entirely: type-directed program synthesis has
pruned search with types for decades.

**Honest limit on that value signal.** "No type error yet" is not "this prefix
completes to a solution." It is an optimistic local heuristic, not a true value
function — a prefix can typecheck and still be a dead end. That is the normal
condition for MCTS heuristics and is what rollout statistics are for, but it
should not be oversold as an oracle at interior nodes the way it genuinely is at
leaves.

**Use it offline, not at inference.** Tree search multiplies inference cost by
nodes expanded, which is very likely fatal on the VF2 target at 20–50 tok/s. But
that figure is **ballad's, not the ladder's** — decode at int8 is bandwidth-bound
at roughly a byte of weights per parameter per token, so drivel at ~1M is about
30× cheaper per token than ballad at ~30M. The verdict stands for the deployed
rung; it is not a ladder-wide law, and a tiny model with a search or scratchpad
budget is a different calculation nobody here has run. If that calculation ever
matters, the shape to check is a **small model whose output is a constraint
rather than text** — a derivation consumed by the decode mask and never shown,
so its latency is precomputation at line starts (the same snapshot trick the
continuation oracle already uses) rather than tokens a human waits on. Note this
is not a licence to revive §2: whatever such a model emits must still be trained
from search, not from the checker's derivations. Off the critical path either
way.

As a **training-data generator**, by contrast, the cost is paid once, on the
host: search finds good programs, the model is fine-tuned on what search found,
and the deployed model does plain greedy or constrained decoding. That is
AlphaZero's expert-iteration shape and it fits this project far better than
search-at-inference.

**Why this matters beyond capability:** it is a second source of *manufactured
search traces* (§5), and its wrong turns are **real** — genuinely explored and
genuinely abandoned by a value signal, rather than constructed by running a
corruption backwards. The two sources have opposite failure modes (§5's risk is
being too tidy; §9's is being too tied to whatever the checker can see), so
building both and comparing is more informative than either alone.

---

## 10. Legibility as a measurable cost

An honest correction to a framing used earlier in this document's development.
R1-Zero's chains were illegible — language-mixing, unreadable — **and its AIME
pass@1 went from 15.6% to 71.0% in the same run.** The mess and the enormous
gain coexisted. It is not true that unconstrained chains simply degrade.

What can and cannot be attributed:

- The 15.6→71 gain belongs to the RL as a whole, **not** to language mixing.
- What *is* attributable to mixing is the small performance drop when a
  language-consistency reward was added to the shipped R1 — a real but modest
  tax, published openly.
- **What those messy chains were actually doing is unresolved.** Nobody knows
  whether it was efficient encoding or noise that happened not to hurt. Claims
  in either direction are unsupported.

**The safety argument does not apply at our scale.** CoT monitorability is a
genuine and fragile property at frontier scale, but a ≤30M model emitting Stitch
has no deception and no situational awareness. Do not import that argument here.

**Legibility still matters here, for different reasons:** debuggability (an
illegible chain means you cannot say why a rung regressed) and the provenance
thesis (§15).

**And there is an opportunity hiding in the small scale.** Frontier labs can
report one data point — "slightly degraded." At 30M parameters the whole sweep
is affordable: vary a legibility constraint from none to strict, measure the
capability tax at every setting, and publish the **curve**. That result is
available *because* the model is small, not despite it, and it stands on its own
whether or not RL improves capability here.

---

## 11. Latent reasoning and the discrete bottleneck

Exploration, not plan. Orthogonal to the RL branch — testable independently.

**The bottleneck.** Between forward pass *t* and *t+1*, the hidden state
computed while predicting token *t* is **discarded**. A d_model-dimensional
residual stream is compressed to one sampled token — roughly 15 bits — then
re-embedded. (Prior positions persist in the KV cache; the newly computed
prediction state does not, except through the token.) The reasoning chain runs
over a very narrow channel between consecutive computations.

Two distinct things the token stream is doing, both real:

- **Token count is serial compute.** More tokens buy more sequential depth,
  which a fixed-depth network cannot otherwise reach. This is why R1-Zero's
  chains grew *longer* under training with nobody rewarding length.
- **Token identity is handoff fidelity.** A better projection of the internal
  state loses less per step.

These pull in opposite directions, which is why "find the densest notation" is
not obviously the right objective: maximal compression buys fewer forward passes
and therefore less computation.

**Widening the vocabulary is a bad lever.** Bits per step grow logarithmically —
100k → 1M buys about three bits. Continuous handoff buys thousands of
dimensions. The instinct toward denser notation points past itself, at
abandoning discreteness.

**COCONUT (continuous chain-of-thought).** Feed `h_t` directly in as the next
input embedding: no unembed, no sampling, no re-embed. `<bot>`/`<eot>` tokens
bracket a latent region; outside it, normal language. Requires a curriculum
(progressively replace language reasoning steps with continuous ones) and loses
transformer training parallelism inside the latent region, so it is not cheap.
Reported roughly competitive on arithmetic with fewer thinking tokens, and
better on planning-flavoured logical reasoning.

**Its mechanistic finding is the interesting part:** the continuous state
encodes **multiple candidate next steps simultaneously** — a breadth-first
frontier held in superposition, with commitment deferred. Discrete sampling must
commit to one path immediately. So the win is not bandwidth, it is *deferred
commitment* — which is also why it helps most on search-shaped problems. Note
the convergence with §9: both are ways of not committing early.

**Filler / pause tokens are weaker than they sound.** They are not nothing — a
filler token's KV entry is computed from context and is attendable by later
positions, so it adds computation. But because the token's *identity* is fixed,
no information routes through the choice. **They buy parallel width, not serial
depth**, which is the thing real CoT provides. Empirically they help on specific
parallelizable algorithmic tasks, require dense targeted supervision to elicit
at all, and generally need to be present during pretraining rather than added at
finetuning.

**Reserved notation tokens — viable here, not elsewhere.** Adding new symbol
tokens for reasoning fails at frontier scale because new embeddings mean
nothing: there is no supervision for what a fresh symbol denotes, and RL cannot
discover one (random embeddings produce garbage, garbage earns negative
advantage, the tokens get suppressed — exploration defeats itself). The missing
piece is *a corpus that gives the symbols meaning*. **We could write one** —
manufacture repair traces (§5) in a compressed notation of our own design,
pretrain so the symbols acquire meaning, then refine. `kvetch-vocab` is frozen
as wire law but frozen *by our decision*; co-designing vocabulary with notation
is a degree of freedom essentially nobody at scale will spend.

**Read-only discretization — the version that fits this repo.** Keep the chain
continuous, but unembed the latent state *on demand* for inspection without
feeding the result back. The model never commits; we observe a projection. This
appears to dissolve the tradeoff: legibility without the capability tax.

The catch is serious and must be printed on the artifact: **a decoded latent
state is a lens, not a log.** It was never optimised for faithfulness and can be
systematically misleading in exactly the way post-hoc explanations are. It is
not a transcript of the computation; it is a reconstruction. (Encouragingly,
COCONUT's authors decoded continuous thoughts and found interpretable candidate
steps, so it is not hopeless — just unwarranted by default.)

Structurally this is the shape this repo already builds: the collector decodes
frames off a wire into observable state; a latent-CoT observer decodes hidden
states into Stitch fragments. Same pattern, one layer down, with an honesty
label attached.

---

## 12. Prerequisites

- **A longer training context.** `TrainingConfig::context` defaults to **128
  tokens** (`cram/src/run.rs`), which is a program-sized window. A repair trace
  (§5) is a broken program *plus* a complaint *plus* a fix, several times over —
  comfortably several hundred tokens. **Increment 0 is invalid until corpus B
  fits**, because a truncated trace is not a worse trace, it is a different task
  (see §14). Favourable facts: positions are RoPE, computed per sequence length
  (`kvetch-model/src/lib.rs`), so raising context adds **no parameters and no
  checkpoint change**; and total corpus tokens are unchanged — you get fewer,
  longer windows, not more data. The cost is per-token attention: at drivel's
  `d_model = 128` the score matmuls are ~`2·T·d` against a ~`8d²` feed-forward,
  so they are negligible at `T = 128` and roughly comparable to the FFN at
  `T = 512`. A small multiple on step cost, not an order of magnitude — but
  measure it rather than trusting that sentence.
- **KV cache in `kvetch-model`.** Does not exist today; generation is O(n²). RL
  is sampling-dominated (5–20× more forward passes than SFT per gradient step),
  so this is the load-bearing cost item. **It is worth building regardless** —
  it is also the on-target inference path for the VF2 runner. Note it composes
  with the item above: cache size is linear in context, so a 4× context bump is
  a 4× cache.
- **Batched sampling** with per-token logprob capture.
- **A corruption engine** shared with the Stitch mutation tester (designed,
  unbuilt). See [../plans/stitch-native-tests.md](../plans/stitch-native-tests.md)
  for the test-syntax half.
- Already present: `eval_program_with_fuel`; `cram_eval::{Predictor, Generator,
  score, parse_rate}`; `cram_corpus::generate`; effect handlers through the
  Stitch pipeline; the type checker
  ([../plans/stitch-type-system.md](../plans/stitch-type-system.md) — Stage 1 and
  contract subtyping done).

---

## 13. Increments

**Increment 0 — the gate. Pure SFT. No policy gradient, no KV cache, no new
trainer math.**

Train two rungs identically except for the training corpus:

- **A:** programs (the status quo).
- **B:** manufactured repair traces (§5).

**Both arms must run at a context that fits corpus B** (§12) — a shared setting,
raised once, used for A as well. A and B differing in context is not the
experiment: it confounds "traces teach repair" with "longer windows help," and
the honest read of a B win would be unavailable. Raising context for A too costs
one extra `cram` run and buys an uncontaminated comparison; measure B's token
length distribution first and set the context above its tail, not its mean.

Score both on held-out corruptions using the existing `cram-eval` harness —
held-out masked NLL (the gate metric) via `Predictor`, plus typecheck-pass on
generated repairs via `Generator`. Note the floor is uniform-over-legal, not
babble (2.758 vs 5.405 free-nll), so compare against the right baseline.

If B does not beat A, **the reasoning branch of this document is answered
negatively for the cost of two `cram` runs** — and answered by our own model at
our own scale rather than by a paper about 100B-parameter ones. If B wins, we
have found this scale's version of the effect and everything below becomes
worth building.

**Increment 1 — measurement.** Pass rate per rung × oracle {parse, typecheck,
runs, tests}. Does anything land in the 5–60% band? Sweep corruption count `k`
and record the difficulty→pass-rate curve. This is what tells us the curriculum
dial has range.

**Increment 2 — KV cache + batched sampling.** Needed for everything after, and
for on-target inference anyway.

**Increment 3 — STaR against the typecheck oracle.** Establishes oracle
plumbing, the rollout loop, and the telemetry, using the existing loss.

**Increment 4 — GRPO**, only if Increment 3 plateaus. Reuses all of Increment
3's infrastructure; adds group sampling and advantage weighting.

**Increment 5 — effect-trace reward** (§6) and the mutation-kill reward at
oracle rung 4.

**Off the critical path, independently testable.** These do not gate each other
and none gates the increments above:

- **Typed MCTS as an offline trace generator** (§9). Needs the KV cache
  (Increment 2) and the corruption/synthesis harness, then produces a second
  trace corpus to A/B against §5's. Strongest candidate after Increment 0.
- **The legibility cost curve** (§10). Sweep a legibility constraint, measure
  the capability tax at each setting. Stands alone as a result.
- **Latent reasoning probes** (§11). Filler tokens are the cheapest thing to
  falsify; COCONUT-style continuous thought is the expensive one; read-only
  discretization is the observability payoff and depends on the latter.

---

## 14. What would falsify this

Stated up front so the design can lose honestly:

- Increment 0 shows repair traces ≤ plain programs. → The reasoning branch dies.
  **Rule out the mechanical explanations before believing it:** were traces
  truncated by the context window (§12), and did both arms train at the same
  context? A gate that fails because corpus B did not fit has answered a question
  about `TrainingConfig`, not about reasoning, and it is the cheapest way for
  this document to be wrongly abandoned.
- Increment 1 finds no rung and no `k` with pass rates in the learnable band. →
  GRPO cannot get traction; STaR may still work.
- Gains appear but do not survive a held-out distribution. → We learned the
  generator, not Stitch (§4 caveat), and the curriculum needs redesign before
  anything else.
- Sampling cost dominates such that a full RL round exceeds a full pretrain. →
  Not worth it at this scale; stop at STaR.
- Search-generated traces (§9) do not beat corruption-generated ones (§5), and
  neither beats plain programs. → Manufactured search is the wrong idea, not
  just the wrong manufacturing method.
- Measured branching factor under constrained decoding turns out far above the
  ~16 implied by the eval floor on the program shapes we actually sample. → The §9
  tractability argument weakens and MCTS returns to the deferred list.

---

## 15. Why this belongs in SnitchOS

The capability payoff is modest and §1 says so plainly. The reason it fits here
is different: **the training loop becomes an observable object.**

Every rollout is a span. Every reward is a metric. The advantage distribution,
the group variance, the pass-rate-vs-difficulty curve, the effect traces of
generated programs — all of it goes out the same wire format as the kernel's,
into the same collector, onto the same dashboards. "Watch a policy learn to
typecheck, live" is squarely this repo's genre, and it is available *because*
the trainer is hand-written; framework users structurally cannot instrument at
this resolution.

It also sharpens the provenance argument from [llm-design.md](llm-design.md): a
model trained by an oracle we wrote, on a corpus we generated, with the full
reward trace recorded and replayable, is an end-to-end attributable pipeline.
That claim is available to approximately nobody else.

---

## 16. Deferred / explicitly not doing

- **PPO and PRMs** (§8).
- **MCTS at inference** — the cost multiplies by nodes expanded and the VF2
  target cannot afford it. Offline trace generation only (§9).
- **Parse-rate as a reward** (§3) — use constrained decoding instead.
- **Any plan depending on emergent reasoning at ≤30M parameters** (§1).
- **Training on checker-emitted derivations** (§2).
- **Importing the CoT-monitorability safety argument** (§10) — real at frontier
  scale, not applicable here. Legibility is defended on debuggability and
  provenance grounds instead.
- **Treating a decoded latent state as a transcript** (§11). Lens, not log.
- **Distillation from a frontier model** — it would work, and it would make the
  provenance claim in §15 worthless. Deliberate refusal, not an oversight.
