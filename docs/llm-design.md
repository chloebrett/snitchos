# LLMs on SnitchOS: kvetch, the continuation oracle, and provenance

**Status:** 📐 **DESIGN — exploration, not started.** Captures the 2026-07-25
design conversation end-to-end: what a language model can do on the VF2, what we'd
train, how we'd synthesize a corpus for a language with zero natural corpus, and
the parts nobody else can build — grammar-constrained decoding off our own parser,
kernel-level authority checking of generated code, and OS-level provenance for
model-written bytes. Nothing here is committed work; the sequencing section says
what the keystone increment would be if any of it is greenlit.

Related: [language-design.md](language-design.md) (Stitch),
[stim-design.md](stim-design.md) (the editor this integrates with),
[clipboard-design.md](clipboard-design.md) (the provenance substrate),
[manifest-design.md](manifest-design.md) (authority description — the static twin
of the Check oracle), [supervision-design.md](supervision-design.md) (agent
lifecycle), [observability-design.md](observability-design.md) (wire format the
telemetry angles ride on).

---

## Headline conclusions

- **A 30M-param model is genuinely interactive on the VF2 (~20–50 tok/s); anything
  ≥1B is a slideshow (~1–2 tok/s), 3B+ academic.** Batch-1 decode is memory-bound;
  the JH7110's ~2 GB/s makes the napkin math reliable. The sweet spot is 10–100M.
- **Zero-FP is a feature.** Fully-quantized int8 inference (int32 accumulators,
  fixed-point softmax/RMSNorm — same LUT discipline as the PWMDAC volume work)
  needs no FPU context switching, which the kernel doesn't have.
- **The unfair advantages are all substrate:** we own the parser (grammar-constrained
  decoding + a continuation oracle with four consumers), the type checker and
  interpreter (best-of-N with real execution as the verifier), the kernel
  (authority checking of generated code, cap-confined agents), and the telemetry
  spine (provenance that survives at OS granularity — a paper-shaped claim).
- **Corpus, not compute, is the hard part.** Training is hours on an M1 Max; the
  synthetic-Stitch corpus pipeline is the open-ended project.

## Performance envelope (VisionFive 2, 4 GB variant)

Hardware facts: 4× U74 @ 1.5 GHz, dual-issue in-order, **no RVV** — pure scalar.
Realistic sustained int8 matmul across 4 harts: ~4–6 G-ops/s. LPDDR4 measured
bandwidth ~1.5–3 GB/s (the JH7110 memory controller is the pole). 4 GB caps model
size at ~3B Q4, but bandwidth makes that moot.

Governing equation for batch-1 decode (every token touches every weight once):

```
tok/s ≈ min( bandwidth / model_bytes , compute / (2 × params) )
```

| Model | int8 size | BW-bound (2 GB/s) | Compute-bound (~4 Gops/s) | Realistic |
|---|---|---|---|---|
| 30M (TinyStories class) | 30 MB | ~65 tok/s | ~65 tok/s | **20–50 tok/s** |
| 125M | 125 MB | ~16 | ~16 | ~8–15 |
| 1.1B (Q4) | ~600 MB | ~3 | ~2 | ~1–2 |
| 7B (Q4) | ~3.5 GB | ~0.6 | ~0.3 | ~0.2–0.3 |

The 7B row matches reported llama.cpp-on-VF2 numbers (~0.2–0.3 tok/s) — sanity
check on the napkin. 30M sits at the rare point where the board is simultaneously
bandwidth- and compute-bound; above that it's pure bandwidth and hart-parallelism
stops helping.

**Prefill is the latency trap.** Decode is fine, but prefill is compute-bound and
scalar: re-encoding a 500-token buffer from scratch is seconds. Autocomplete only
works with incremental KV-cache reuse (see the versioned-buffer protocol below).
This is an architecture decision for the runner, not a later optimization.

## The runner (working name: kvetch)

A userspace inference engine, llama2.c-shaped:

- **int8 weights, int32 accumulators, fixed-point activations** (softmax, RMSNorm
  via LUT + fixed-point — no FP anywhere). Runs on `snitchos-user` as-is; no FP
  trap-state save/restore ever needs to exist.
- **Weight delivery** is the first real systems problem: a 10–30 MB blob doesn't
  ship via `include_bytes!` comfortably; wants `MapAnon` + the RAMfs
  ([filesystem-design.md](filesystem-design.md)) to carry it.
- **Multi-hart matmul**: row-partitioned across harts using existing IPC/
  notification primitives; the cross-hart Release/Acquire oracle work already
  proved the ordering story.
- **Serves completions as an IPC endpoint** — same `Call`/`Reply` loop as the FS
  server. The client holds a `Send|Call` cap on a completion endpoint and neither
  knows nor cares what's behind it (see "Local or network" below).
- **Develop under snemu first**: deterministic, itest-able ("model emits ≥N tokens
  and heartbeat survives" is a scenario like any other), and `snemu profile` hands
  us the hot-loop breakdown. snemu at a few hundred MIPS runs the 30M model at
  ~1–5 tok/s — slow but fine for correctness.
- **Increment zero is the existing TinyStories 30M checkpoint** — a known-good
  model that proves the fixed-point kernels and weight-delivery path before any
  Stitch-specific work exists.

**Naming reservations:** **kvetch** = the model subsystem + inference engine (the
KV pun is the bonus layer; the real fit is that this OS's personality is telling
on itself). **hunch** = shelved for the completion service if it ever wants its
own name. **tattle** = reserved for the provenance layer — kvetch generates,
tattle tracks.

## The model

A Stitch code-completion model, not a chat model. Honest calibration: at 10–30M
params it is a *syntax-shape and idiom suggester* — completes the current line,
closes match arms, suggests `use M.{a,b}` patterns — not a semantic reasoner.
Two things raise the ceiling well above "useless":

1. **The training distribution is correct-by-construction** (every corpus program
   parses, type-checks, passes tests), and TinyStories showed 30M models do
   in-distribution generalization, not just recall. Idiom-sized logic will often
   be right.
2. **Correctness by search**: generate N candidates, filter through the
   verification stack (below). Tiny model as proposal distribution, our
   infrastructure as oracle. Single-shot correct logic: no. Best-of-N with
   verifiers: genuinely yes, for small functions.

Design choices:

- **Small BPE vocab (~2–4K).** Stitch's keyword set + our naming conventions;
  frees the embedding budget for the trunk, and makes constrained-decoding
  vocab-masking trivially cheap (test all 4K entries per step, done).
- **FIM (fill-in-middle) training objective** — that's what makes it an
  autocompleter rather than a continuator.
- **Context length 1–2K**, no longer than the VF2 can afford to serve.
- **Size sweep 5/15/30M**; expect the knee below 30 for a single small language.

### Training cost (M1 Max baseline: 30M TinyStories = 9 h)

Compute ≈ 6 × N × D. The Stitch run moves both factors down: corpus ~20–100M
tokens (vs ~500M) × up to ~4 epochs (the data-constrained-scaling knee), model
plausibly 10–15M. **Guess: 2–5 h/run on the M1 Max — but budget 8–12 runs**
(tokenizer sweep, size sweep, FIM-ratio ablation, one corpus regeneration).
Levers, in order: corpus tokens (linear, dominant unknown), params (linear),
epochs (≤4 nearly free), vocab (two-sided: smaller vocab = smaller embeddings but
more tokens/char), context (quadratic attention, +30–50% at 1K), hardware (a
rented A100 turns a run into ~15 min for ~$1 — the real lever is iteration
speed). Data quality moves the tokens-needed multiplier: a validated, deduped
corpus is why 30–50M tokens might suffice where scraped code needs 10×.

**Training on the VF2 itself: no.** ~10¹⁷–10¹⁸ FLOPs at scalar GFLOPS is decades.
Train on the host, quantize, ship the blob.

## Corpus synthesis (the actual hard part)

Stitch has zero natural corpus; the pipeline is tiered so frontier-model tokens
are the anchor, not the volume:

| Tier | Source | Tokens | Cost |
|---|---|---|---|
| 0 | Grammar-directed sampling + augmentation | ~10M | ~$0 |
| 1 | Opus seed set (quality anchor + few-shot library) | ~1M | ~$100 |
| 2 | Cheap model (Flash/Haiku) bulk with ICL, ~60% validated yield | ~30M kept | ~$40–80 |
| 3 | (later, optional) kvetch self-distillation under constrained decoding | — | — |

vs ~$3–4K all-Opus for the same volume.

- **Tier 0**: we own the AST — sample it directly, biased toward shape statistics
  measured from real code (CSmith-style probability tables). Perfect for syntax,
  semantically vacuous; cap at ~20–30% of the corpus. Plus semantics-preserving
  augmentation of every validated program (alpha-renaming, reordering,
  extract/inline) — a free 2–4× multiplier, validator-checked.
- **Tier 2 disciplines**: context caching (the ~8–10K-token spec+exemplar prompt
  is identical across tens of thousands of calls); track validated **yield per
  prompt recipe** — a 20%-yield recipe means the cheap model doesn't understand
  that idiom → add a targeted Tier-1 exemplar for it (spot-repair with the
  expensive model only where the cheap one fails).
- **The validators are the equalizer**: parse → type-check → run tests → dedup.
  Rejection is free and automated; we pay for candidates, not corpus.
- **Tier 3 caveat**: model collapse (Shumailov et al. 2023); our
  validation+constrained-decoding stack is the published mitigation, but don't
  plan on this tier.
- **Main risk is homogeneity, not cost**: a corpus that's 75% one cheap model's
  idea of Stitch. Mitigations: the diversity scheme below, the augmentation tier,
  and periodic Opus-judging of random samples against the seed set for idiom
  drift (pennies). If drift appears, tighten exemplars, don't upgrade the bulk
  model.

### Diversity: injected via random seeds, never requested

1. **Structured recipe tuples** (the TinyStories §2.1 mechanism, upgraded):
   per-call sample of domain (~300-entry noun list) × 2–3 required Stitch
   constructs × size bucket × shape (module/script/server loop) × **3 random
   must-use identifier words** — the last is what forces the long tail.
2. **Swarm-style feature masking** (Groce et al.): per *batch*, ban a few common
   features to push generation into rare-construct territory.
3. **Snippet grounding** (OSS-Instruct/Magicoder): sample random snippets from
   real code in other languages → "express this intent in Stitch." Imports real
   program-shape statistics instead of the model's imagination.
4. **Depth via evolution** (Evol-Instruct): mutate validated programs' prompts —
   "add a failure path," "generalize to N," "split into two modules."
5. **Measure, don't vibe**: **grammar-production coverage** (we own the parser —
   a diversity metric nobody else gets cheaply), MinHash near-dedup rate computed
   on the **alpha-normalized** token stream (so renamings count as dups),
   identifier entropy. Per-batch report; when production coverage plateaus, that
   recipe axis is exhausted. The corpus pipeline snitches on itself.

### Literature anchors

Synthetic-data line: TinyStories (Eldan & Li 2023, §2.1 = the random-words
mechanism); phi-1 "Textbooks Are All You Need" (Gunasekar et al. 2023 — names
diversity as *the* problem); Self-Instruct (Wang et al. 2022 — seed-then-bootstrap
+ overlap filtering); Evol-Instruct/WizardCoder (Luo et al. 2023); OSS-Instruct/
Magicoder (Wei et al. 2023); SelfCodeAlign (2024 — execution-validated loop,
closest published pipeline to ours); Lee et al. 2021 (dedup); Muennighoff et al.
2023 (≤4-epoch knee); Shumailov et al. 2023 (collapse). Compiler-testing line:
CSmith (Yang et al. 2011 — probability-table steering); Swarm Testing (Groce et
al. 2012); YARPGen (Livinskii et al. 2020); Fuzzing Book grammar-coverage chapter.
**The gap = our opportunity**: compiler-testing generators optimize validity+edge
cases, LM-corpus papers optimize naturalness+diversity; nobody has published the
hybrid — grammar-guaranteed validity, LM-shaped naturalness, execution-validated,
production-coverage-measured. Plausibly paper #2 (provenance is #1).

## The continuation oracle: one function, four consumers

There is no static regex for "valid Stitch" — validity is a **function from
prefix → valid-next-token set**; *given* a prefix, valid next characters form a
regular language (union of the legal tokens' lexeme regexes + "continue the
partial token"). Build it once in the stitch crate as a pure, host-tested
function of (text, position):

```
valid_next(parse_state) → TokenSet        (+ lexer layer → char classes)
```

- **Extraction — oracle by replay**: parse the prefix with a sentinel token;
  instrument every `expect`/`peek` so that hitting the sentinel records which
  token kinds would have been accepted, unwind, try alternatives. O(prefix) per
  query; for keystroke rate, snapshot parser state at line starts (tree-sitter's
  trick, miniature) and re-parse from the nearest snapshot. This is the "expected
  one of X, Y, Z" error message promoted from string to API.
- **Lexer wrinkle**: maximal munch (the call-paren gotcha) means char meaning
  depends on lexer lookahead — the char oracle is (parser token set) × (lexer
  state). Write that one test.
- **BPE lifting**: constrained decoding needs "which vocab entries extend a valid
  lexical+syntactic prefix." Published systems (Synchromesh, Outlines, llguidance)
  exist because this is hard against 32–128K vocabs; our 2–4K vocab makes naive
  per-step testing of every entry affordable. Known caveat: mask+renormalize
  subtly distorts the model's distribution (grammar-aligned decoding, 2024) —
  ignorable at our scale, but named.

**The four consumers** (one oracle, one set of bugs, all in lockstep):

1. **Decoder mask** — kvetch can only emit syntactically valid Stitch; a 10M
   model that can't utter garbage feels dramatically smarter than it is. The type
   checker can narrow the mask further (only expressions of the expected type).
2. **stim affordances** — completion menu = the valid token set ranked by kvetch
   (grammar supplies *legal*, model supplies *likely*: the menu is never wrong,
   only variably helpful); **forced-token auto-insert** when the set is a
   singleton (closing delimiters, mandatory keywords — deterministic, zero-model
   autocomplete for the forced moves). **Never block keystrokes** — hard-reject
   structure editors have failed since the Cornell Program Synthesizer; humans
   legitimately type through invalid states, and the human has a suffix below the
   cursor that prefix-validity ignores. Keystroke-level strictness stays an
   opt-in UI policy, not machinery.
3. **Diagnostics** — the expected-set drives actionable squiggles via the same
   code path as the decode mask.
4. **Live parse-state highlighting** — a **trichotomy**, not a binary:
   - **Parsed** (normal): token consumed by a completed production; committed.
   - **Viable** (yellow/dim): incomplete but extendable — some suffix makes it
     legal. The normal state of typing.
   - **Dead** (red): *no possible continuation* is legal — proved by the oracle,
     not heuristics. Red only when it's true, so it stays trustworthy.
   Blast-radius control via recovery/synchronization points (statement
   boundaries) so one dead token doesn't paint the file red; short debounce
   (~300ms) before showing red; fold the maximal-munch "committed lexically,
   pending syntactically" state into viable for v1. Bonus: the highlighter
   exercises `valid_next` on every keystroke — the editor becomes the oracle's
   fuzzer, and it's a live visualization of how parsing works (learning-track
   gold).

## The verification stack: four verdicts, no effect performed

A candidate completion passes through a gradient of checkers, each cheaper to
fail than the next:

1. **Grammar** — constrained decoding; can't be malformed by construction.
2. **Types** — the bidirectional checker.
3. **Authority** — run the candidate in the interpreter's **suspended-effects
   mode**: anything effectful (IPC send, FS write through a cap) is *recorded,
   not performed* — the effect-handler-membrane idea from
   [stim-design.md](stim-design.md) pointed at a candidate completion. Immutability
   makes this transactional: the verdict is a pure function of (candidate,
   tests). Then map each recorded effect to a syscall shape and batch-check it
   against the **kernel Check oracle** (below). "This completion would be refused
   by the kernel" as a compile-time-style error.
4. **Behavior** — actually run it against tests in the interpreter. Cheap, and
   safe *because* execution of untrusted generated code in a cap-confined process
   is the OS's entire design. Most pipelines can only check pure functions; ours
   checks effectful ones because effects are reified.

### The kernel "would this be approved?" oracle

A `Check(syscall, args)` entry point (or hypothetical flag on dispatch) that runs
the validation half — handle lookup, generation, rights mask, quota — and skips
the action. Cheap because cap validation is already pure, host-tested
`kernel-proc` logic; the decision function is separable from the effect today.

- **Advisory, and says so.** Approval-now ≠ approval-at-commit (revocation, quota
  changes; generation counters exist precisely so answers can change). An
  *oracle*, not a *reservation* — if something later needs atomicity, that's a
  separate lease/reserve primitive; don't conflate.
- **Probes snitch.** A hypothetical check emits `SyscallChecked { would_refuse,
  reason }` — so *an agent probing the edges of its authority is itself
  observable*. In every other sandbox, permission-probing is invisible
  reconnaissance; here it's a trace. Possibly the most novel security property in
  this whole arc.
- **Completes a gradient**: caps-as-effects in the Stitch type system (static) →
  Check oracle (dynamic, pre-execution) → actual dispatch (enforcement). Also
  quietly serves [manifest-design.md](manifest-design.md) — checking a manifest's
  authority claims pre-spawn is the same oracle at a different moment.

## stim integration: the versioned buffer

Today's editor↔model split (everyone's): stateless model API, so every completion
re-ships the context and the server re-prefills from scratch; all KV work is
discarded per keystroke. The alternative our architecture makes natural:

- The completion server is a **stateful process** holding, per open buffer, a
  chain of KV blocks — the transformer's digested representation of the text.
- stim doesn't send "here's my buffer"; it sends **"buffer 42 → version 108:
  delta = replace bytes 3101..3104"** — which it knows exactly, because edits are
  its native events.
- The server maps the delta onto the KV chain: prefix blocks before the edit are
  still valid; drop the suffix blocks, prefill only from the edit point. A
  keystroke costs ~one token of prefill, not five hundred.
- Version numbers on both sides: a reply says "valid against v108" and stim
  discards it if the user typed past it — no stale-completion races.
- KV storage: paged (vLLM-style, miniature) in `MapAnon` regions. Design wrinkle
  to enjoy later: an edit *before* the cursor invalidates all KV after it, so
  mid-file edits cost more than appends; block-granular drop-and-refill is the
  honest v1.

The model server's KV state is a *materialized view of the buffer*, incrementally
maintained by the same edit stream that drives the screen. **One edit log, many
projections**: undo tree, KV chain, provenance map, deterministic session replay
(the [physics-desktop-design.md](physics-desktop-design.md) replay instinct) are
all folds over it — vs a mainstream editor's N ad-hoc stores.

Further stim angles:

- **Accept/reject is a wire event → training flywheel.** Every ghost-text
  suggestion is a span; acceptance, rejection, edit-after-accept are frames. A
  labeled preference dataset accumulates *as telemetry*, with full context
  (buffer version, parse state, model id). Our own usage trains the next
  checkpoint.
- **Effect-preview on accept**: for function-sized candidates, the
  suspended-effects run + Check oracle lets stim show "this completion would
  write via `fs` cap, send on endpoint 3 — all within authority" *before*
  acceptance. Accepting code and accepting its effects become one reviewable act.
- **Provenance rendering**: gutter marks from actual substrate labels, not diff
  heuristics; "blame by trace" — click a region, see the completion span, model,
  acceptance event.
- **Agent edits as transactions**: a multi-file change arrives as a batch of
  buffer deltas — reviewable as a span tree, applied atomically,
  provenance-labeled agent-authored, every write cap-checked. Model-originated
  input is just another input source routed through a membrane that tags it.
- **All itestable**: scripted keystrokes under snemu, fixed weights, greedy
  decode, assert on the frame sequence. An editor whose AI integration has
  deterministic integration tests.

Sequencing note: stim is paused behind the Stitch core redesign, and the oracle/
membrane angles lean on Core IR + effect handlers — this arc adds weight to
resolving that decision rather than pressure to bypass it. The completion server
and the Check oracle are stim-independent and can land while stim waits.

## Provenance (the paper-shaped one)

Model-produced bytes enter the system as a *tainted source with a trace id*
(the [clipboard-design.md](clipboard-design.md) substrate); every insertion into
a buffer, every write through the FS server, carries it. Then:

> "Which lines of this file were model-written, under which completion span,
> proposed by which model, accepted by which human action?"

is a **query, not a forensic reconstruction**. The publishable claim: **provenance
survives at OS granularity because the OS has no unattributed channels** — every
byte-move is a syscall through a cap, every grant is a wire event, so
"model-written" is a label the substrate *can't lose*, not metadata an app
promises to maintain. Unsolved generally because host OSes have amnesia points
(clipboard, file write) where provenance dies; ours doesn't. Nobody can build
this on Linux without rewriting the world; we get it because the world is already
rewritten. Demo that sells it: a file with interleaved human/model edits, the
query answered from telemetry alone, the Tempo trace as the figure. Working
title: *"Provenance is a substrate property."*

## Local or network, transparently — but not to the observer

The client holds a `Send|Call` cap on a completion endpoint. Behind it:

- **Local**: kvetch serving int8 inference, same serve loop as the FS server.
- **Remote**: a relay process holding the same `RECV` cap, forwarding to the host
  (which can run the *big* model — converges with the collector-as-server
  direction). v1 needs no network stack: the host link exists (virtio-console
  channel); the real network stack is coming and slots in behind the same cap.

init picks which server to spawn and delegates the endpoint — the swap is a
delegation-graph decision, invisible to the client. The snitchos twist:
**transparent to the client, not to the observer.** The completion span shows
which server answered; the cap derivation shows who granted the authority to ask;
a remote completion visibly crosses the trust boundary in the trace. "You can't
tell the difference, but the system can prove which one happened" — the OS thesis
in one feature.

## Contextual help ("help", anywhere)

The user types `help` in the terminal (or stim) and gets a contextually relevant
answer. Framing this as "which model?" undersells the position: **contextual help
is 80% context assembly + retrieval, and the substrate is freakishly good at both
halves.** On a host OS the context is gone (scrape scrollback and guess); here
**the context window assembles itself from telemetry** — the last fault (with
file:line:col + backtrace), the last `SyscallRefused` (cap + reason), the recent
frame ring, the parse state at the cursor (the continuation oracle again), the
process's cap table. "Help" is almost always answerable from structured events
already recorded — typed data to *dispatch on*, not prose to be understood.

Three layers, in order of authority:

1. **Deterministic help cards, keyed by structured events.** A curated knowledge
   base (in the ramfs) indexed by event shape, not keywords:
   `SyscallRefused{NoSuchRight}` → the card explaining rights + who could grant
   this cap + delegation syntax; a parse fault → that production's card *plus*
   the oracle's expected-set rendered as "here you could write: …". Pure lookup —
   testable, offline, instant, and correct by construction. Man pages with a
   foreign key into the telemetry schema. This covers most real help moments,
   because most confusion follows an *event*.
2. **Retrieval over the docs corpus.** No exact card → retrieve relevant doc
   sections (BM25 is plenty at this corpus size) and show them, attributed.
   Still deterministic, still offline. Retrieval targets the **user-facing
   corpus**, not `docs/` — the dev docs carry rationale and history that help
   must never surface; see
   [tour-and-user-docs-design.md](tour-and-user-docs-design.md) for the split.
3. **LLM synthesis, behind the same endpoint pattern as completion.** `help` is a
   cap-mediated endpoint; the client doesn't know what answers. The model gets
   the assembled context bundle + retrieved cards and synthesizes — RAG over
   curated content, not free recall, which keeps it honest. **A cheap model
   (Flash-class) is the right tier**: the grounding does the knowing, the model
   does the phrasing; a few thousand cached input tokens per query is fractions
   of a cent. Provenance marks which layer answered; a remote help query visibly
   crosses the trust boundary in the trace.

Layers 1–2 are the important ones, and not just for cost: **they're the only
layers whose answers are promised true.** An OS whose thesis is "never lies about
itself" should have a help system with the same spine — the LLM tier is
presentation, not authority.

Model options, assessed:

- **Frontier/cheap API over the relay**: Layer 3's default. Zero training cost.
- **Fine-tuned open model (4B-class LoRA over docs + synthetic Q&A)**: viable
  later, host-side only (1B+ at 1–2 tok/s on the VF2 means a paragraph takes a
  minute — dead on device). Strictly the offline/private substitute for the API
  tier; defer until layers 1–2 exist, since they're its grounding either way.
- **From scratch**: not for generation — sub-~300M NL Q&A is fluent and
  confidently wrong, and wrong *help* is worse than none. The tiny-model-shaped
  role is the **router/ranker**: classify the context bundle to the right card or
  doc section. Largely structured input (frame types, fault kinds — not English
  vocab), so a few-M model, or initially hand-written dispatch, does it.

**The browser loop (via snemu-wasm).** A q4 3–4B open model runs in-browser on
WebGPU (WebLLM/MLC class: ~2 GB weights, 15–60 tok/s on a laptop GPU); snemu-wasm's
guest RAM is a few hundred MB — they coexist in a tab. The relay's "network"
becomes a JS shim on the virtio-console channel routing to the in-browser model.
Same delegation graph, third backend: **zero-install, zero-server — the OS, the
emulator, and the model that explains them, entirely in a tab.** The strongest
shareable demo of the help/provenance story. See
[snemu-wasm-design.md](snemu-wasm-design.md).

**Flywheel**: "was this helpful" (or the behavioral proxy — did the next action
resolve the situation?) is a wire event; every unanswered `help` is a logged gap,
so the card set grows where the misses are. In stim, `help` at a cursor is the
same endpoint with a richer bundle (parse state, type expectation at the hole);
help-on-red explains *why* it's dead, from the oracle, deterministically.

## The agent

An O(10M) local model cannot be an agent. A remote big model planning over the
network, *enforced locally*, can — and the harness is mostly already built:

- **Caps solve the sandbox out of the gate**: the agent process holds exactly the
  caps the human delegated for this task (the explicit-authority shell idea —
  each grant an observable CapEvent). Out-of-authority attempts aren't a
  prompt-injection debate; they're `SyscallRefused` on the wire. The failure mode
  is a telemetry frame, not a jailbreak.
- **Tool calls are just syscalls/IPC Calls** — the "tool schema" problem
  dissolves; the tool schema *is* the endpoint protocol.
- **Constrained decoding again**: constrain the model's action output to the
  shell's actual grammar so it can't utter a malformed action.
- **Provenance tracks model actions against human ones**; supervision
  (`WaitAny`, `Kill`, [supervision-design.md](supervision-design.md)) is the
  harness lifecycle for free.
- The Check oracle gives the agent (and its overseer) pre-flight "would this be
  approved?" — and probing is itself observable.

## Sequencing

The angles span runner, corpus, training, stim, provenance, network, agent —
intentional fan-out, but with real dependency structure. **The keystone is the
runner**: pure systems work, needs no corpus/network/stim, and everything stacks
on it.

1. **kvetch v0**: int8 + fixed-point runner for the *existing* TinyStories 30M
   under snemu; weight delivery via RAMfs/`MapAnon`; itest scenario; then the
   board.
2. **Completion endpoint** with a stub or TinyStories model behind it; the
   local/remote swap proved.
3. **Continuation oracle** in the stitch crate (pure, host-tested; four consumers
   land incrementally — diagnostics first, then decode mask, then stim).
4. **Corpus pipeline** (host-side, parallel with 1–3): recipe generator, tiers,
   validators, coverage metrics.
5. **Training runs** (host GPU or rented); quantize; ship.
6. **stim integration** (after the core-redesign decision): versioned buffer,
   highlighting, effect-preview.
7. **Check syscall + suspended-effects verification** (kernel + interpreter work;
   independent of 4–5).
8. **Provenance labels + the paper**; **agent harness** last.

Corpus generation (Opus/API budget, days of wall-clock) dominates the project;
training is the cheap part — the inverse of TinyStories, where data was free and
the 9 hours was the cost.

## Open questions

- Tokenizer: BPE size, and whether identifiers get word-piece treatment tuned to
  our naming conventions.
- How much Tier-0 grammar-sampled data is too much (gibberish-shaped code in the
  training distribution)?
- KV eviction policy for the completion server under the 4 GB (board) / heap
  (guest) budget.
- Where provenance labels physically live (per-byte is absurd; per-edit-span in
  the buffer log is the obvious answer — confirm against the clipboard design).
- Check oracle: distinct syscall number vs a hypothetical flag bit on dispatch.
- Does the highlighter's parsed/viable/dead trichotomy need the fourth
  (maximal-munch "pending interpretation") state in practice?
