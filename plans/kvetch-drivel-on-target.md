# Plan: drivel at the Stitch prompt (kvetch serves weights, not babble)

**Branch**: main (house rule — no feature branches)
**Status**: 🚧 **Steps 1–4 done and green; step 7 partly done ahead of schedule.**
Tab at the on-target Stitch prompt is answered by the trained checkpoint:

```
greet(name) {
    let padded =
```

(babble, for comparison, gives `.. and ..= < "score" +`.)

### What the build actually cost, versus what the plan guessed

| | predicted | measured |
|---|---|---|
| one completion, guest instructions | 0.2–0.5B | **4–8B** (8 tokens) |
| machine RAM | *unconsidered* | 16 MiB default is **too small**; needs 64 |
| snemu support | assumed complete | **`fclass.s` was missing** |

The instruction estimate was wrong by more than an order of magnitude, which is why
the plan wrote it down to be checked. Three things came out of it:

- **A `slow` scenario profile.** Both drivel scenarios are opt-in: excluded from an
  unfiltered run, still runnable by name or `--tag kvetch`. A gate that takes minutes
  is a gate people stop running. `itest-harness` grew `Scenario::opt_in`, and the
  snemu audit path needed the same filter — it selects from `SCENARIOS` directly
  rather than through `select_by_tags`.
- **A `kvetch-drivel` kernel feature**, because the design decision below was not
  enough on its own. A separate *binary* still lands in the same *image*:
  `itest-workloads` embeds every program, so 4.5 MB of weights rode into all 130
  other scenarios' kernels and pushed the 16 MiB machine out of frames at userspace
  load — half the userspace suite began failing `OutOfFrames`, and each failure then
  burned its full step budget, turning a 7-second gate into fifteen minutes. The
  weights are now embedded only when the feature is on, and the itest builds with it
  only when a selected scenario needs it (so it selects *before* it builds). The
  workload registry stays additive: without the feature the workload still exists and
  fails honestly at ELF load.
- **The KV cache landed early** (step 7 work, pulled forward because the numbers
  demanded it): 46.5s → **11.8s** for the six-token REPL completion, ~10× on the
  marginal per-token cost once the fixed ~8s boot is discounted. It is bit-identical
  to re-running the prefix, which is asserted rather than hoped.
- **A `fclass.s` gap in snemu**, found because the guest halted on it. See below.

### Two diagnostic holes, both worth more than the feature

**The itest harness discarded snemu's halt reason.** `if self.machine.step().is_err()`
threw away *what* went wrong, so an emulator that stopped dead on an unmodelled
instruction — the one thing snemu is designed to shout about — reported as "no frame
arrived", and the scenario blamed whatever it was waiting for. It now carries the
reason, and the first run after the fix printed
`Unimplemented { pc: 0x1000409a, instr: 0xe00516d3 }`, which decodes to `fclass.s`.
Minutes of guessing became one run.

**A mispaired server answers rather than dies.** `serve_model` refuses to serve on a
checkpoint/vocab mismatch, but keeps answering `Malformed` forever instead of exiting
— because a client blocked in `call` on a dead endpoint has no refusal and no
timeout, and the symptom surfaces two processes away. Same lesson as
`plans/legacy/fp-context-switching.md`.

## Goal

Tab at the on-target Stitch prompt returns a completion sampled from the trained
`drivel-all-30k` checkpoint, grammar-masked so it is still always legal — with babble
left intact beside it as the rung-0 control.

## What is already true (measured 2026-07-28, not assumed)

- **`kvetch-model` and `kvetch-vocab` build for `riscv64gc-unknown-none-elf`
  unmodified.** `cargo build -p kvetch-model -p kvetch-vocab --target riscv64gc-…`
  is clean in 24s, `libm` included. The on-target runner these crates were written
  for turns out to need nothing from them.
- **Two processes can hold the FP registers**, as of increment 4b this morning. This
  is a precondition, not a convenience: the REPL lexes float literals and the model
  does f32 matmul, so under the old one-holder rule a weights-backed kvetch would
  have been killed on its first `fmul` while the REPL held the unit.
- Per-request seeding (`kvetch_proto::request_seed`) and the Stitch oracle are
  already wired into the server; babble's `handle_request` is the shape to mirror.
- **drivel is 1,049,728 params** — `d_model` 128, 4 layers, 4 heads, ffn 512, vocab
  2048 — which is exactly the 4,198,948-byte `drivel-all-30k.kvetch` on disk.

## Design decisions

### A separate server binary, not a flag on the existing one

`kvetch-drivel-server` is its own ELF and its own workload (`stitch-drivel`), leaving
`kvetch-server` (babble) untouched. Three reasons, in order of weight:

1. **The default kernel image must not grow by 4.2 MB.** Every itest boots the
   `itest-workloads` build; embedding weights in the one existing server would tax all
   130 scenarios for the benefit of one.
2. **A/B for free.** Same REPL, same harness, same prompt — the only difference is
   which server is behind the endpoint. That is the cleanest possible comparison of
   rung 0 against rung 1, and it is the comparison the whole ladder is about.
3. Babble stays the fallback that needs no checkpoint, which matters for the next
   decision.

### One checkpoint gets committed, and stops being a derived artifact

Checkpoints are gitignored because they are derived — what the repo pins is the
generator, the config and the seed. **This one becomes an exception**, on the same
grounds as `fs-image/`: once a checkpoint is embedded in a program the kernel boots,
it is not a training by-product any more, it is a *contract*. A fresh clone must build
the same guest, and a scenario asserting a byte-exact completion is asserting against
these weights specifically.

So `checkpoints/drivel-all-30k.{kvetch,vocab}` are committed (4.2 MB + 7 KB) and the
build depends on them unconditionally — no conditional `build.rs` row, no skip path in
the scenario, no "works on this machine only". That removes a whole class of
almost-right states.

**Mechanically: `git add -f` those two files, leaving `/checkpoints` ignored.** This is
already the intended route — `.gitignore:27` reserves it ("force-add that one file when
it exists rather than un-ignoring the directory"), and it stays right for the same
reason it was written: un-ignoring the directory would sweep in every future
experiment, and the value here is that *one* pair is blessed. The plan widens that
reservation from one file to a pair, because a checkpoint without its vocab is not a
fixture, it is a hazard (see the fingerprint decision).

What it costs, stated so the next person weighs it deliberately: **4.2 MB per pinned
version, forever, in git history.** Pin *one* pair, replace it rarely, and when
replacing, replace both halves in the same commit (see the fingerprint decision
below). A ladder rung whose weights change weekly does not belong in git — this is the
one we serve, not every one we train.

Unit tests still build a tiny synthetic `Model` by hand rather than loading the real
one: a 4.2 MB fixture and a forward pass in a unit test buys nothing over twelve
tokens and two layers, and the fast tests are the ones that get run. The committed
checkpoint is what the *itest* and the host-side recomputation (step 5) use.

### Weights are embedded, not read from the filesystem

`include_bytes!` into the server ELF. The alternative — reading the checkpoint off the
seeded RAMfs — needs a *second* capability (the FS endpoint) in a process that already
spends its one endpoint slot on receiving completion requests. That is the manifest
work, and it is not worth blocking this on.

Cost to be aware of: the largest embedded ELF today is `stitch_repl` at 623 KB, so a
~4.35 MB one is ~7× outside anything the ELF loader and W^X page planner have been
asked to do. Step 3 is where that gets found out.

### The checkpoint records which vocab it was trained with, by fingerprint

`Model::new` already rejects a weight count that disagrees with its config, on the
grounds that a silent mismatch yields plausible garbage rather than an error. A
**vocab** mismatch is worse: silent, and not caught by any shape check. Pair
`drivel-all-30k.kvetch` with a different 2048-token vocab and every array is the right
size, every index is in range, and the output is fluent-looking nonsense with nothing
anywhere reporting a problem.

**Counting tokens does not catch this.** Vocab size is a training hyper-parameter that
barely varies — 2048 is the frozen figure for the whole ladder — so "both have 2048
entries" is true of every mispairing we could actually make. The check has to be over
*content*: the merge list and its order, which is the part that decides what a token
id means.

So: `cram` writes a **64-bit FNV-1a fingerprint of the serialized vocab into the
checkpoint header** at save time, and the server recomputes it over the vocab it
embedded and refuses to serve on mismatch. The checkpoint asserts its own provenance,
rather than something downstream asserting a coincidence. FNV-1a because it is a dozen
lines of `no_std` with no dependency, and because this guards against an accident (a
half-updated pair, a copied filename), not an adversary.

Two consequences to plan for:

- **The header gains a field, so the format version bumps and existing checkpoints do
  not load.** Rather than retrain, `cram` gets a one-shot stamp path that reads a
  checkpoint + the vocab it was trained with and writes the fingerprint in. That is
  cheaper than a retrain and keeps `drivel-all-30k`'s measured numbers valid.
- A fingerprint over *serialized* vocab bytes covers merge order for free, which is the
  half of "wire law" a token count silently ignores.

### The legality guarantee survives the switch

babble's contract is that a completion is a *fragment* that leaves the buffer viable.
Weights do not get to weaken that: logits are masked by `oracle::valid_next` before
sampling, so an illegal token cannot be drawn at all. The model chooses among legal
continuations; it never decides what is legal. (`ModelCompleter` re-validates client
side regardless — that stays.)

### Inference-only forward is deferred, deliberately

`forward_with` is `trace_with(…).logits`: it computes logits for **every** position and
retains every training intermediate, and without a KV cache each generated token
re-runs the whole prefix. Serving wants none of that. But it is an *optimisation*, and
the instruction from the desk is end-to-end first, measurement after — so it lands in
step 7, informed by step 6's numbers, rather than being guessed at now.

## Acceptance criteria

- [ ] Tab at the on-target Stitch prompt under `workload=stitch-drivel` inserts a
      completion sampled from the trained checkpoint.
- [ ] Every completion is legal Stitch: the sampler cannot emit an oracle-rejected
      token, pinned by a test that would fail if the mask were dropped.
- [ ] Same seed + same prefix ⇒ same bytes, verified against a host-side recomputation
      (the `kvetch-babble-serves` pattern, with `kvetch-model` as the oracle).
- [ ] Repeated Tabs work — the FP-contention case that killed the babble server.
- [ ] A checkpoint paired with the wrong vocab is **refused at startup**, not served,
      and the refusal says which pair disagreed.
- [ ] babble still serves `workload=stitch-kvetch`, unchanged.

## Steps

Every step follows RED-GREEN-MUTATE-KILL MUTANTS-REFACTOR. No production code without
a failing test.

### Step 1: Grammar-masked sampling, as a pure function

**Acceptance criteria**: given logits, a set of oracle-legal token ids, and a seed,
the sampler returns a legal token; an illegal token is never returned however large
its logit; the same seed returns the same token; an empty legal set returns `None`
(the caller stops rather than emitting rubbish). Lives in a new `kvetch-serve` crate
(`no_std` + alloc), host-tested, no checkpoint.
**RED**: `a_token_the_oracle_rejects_is_never_drawn_however_confident_the_model_is` —
hand a synthetic distribution its favourite token *outside* the legal set and assert
it comes back with a legal one. This is the test that fails if the mask is ever
dropped for speed.
**GREEN**: mask → softmax → seeded draw.
**MUTATE**: `cargo xtask mutants kvetch-serve`.
**Done when**: criteria met, mutants addressed, human approves commit.

### Step 1b: The checkpoint carries its vocab's fingerprint

**Acceptance criteria**: the checkpoint header holds a 64-bit FNV-1a fingerprint of the
serialized vocab; `cram` writes it at save time; `Model::decode` exposes it; a
one-shot stamp path writes it into `drivel-all-30k.kvetch` without retraining. Two
vocabs of identical size but different merge order produce different fingerprints —
which is the whole point, and the test that proves it.
**RED**: `two_vocabs_of_the_same_size_but_different_merges_fingerprint_differently`,
built from two hand-made 2048-entry vocabs. It fails against any size-based check,
which is what makes it worth writing.
**GREEN**: the hash, the header field, the version bump, the stamp path.
**MUTATE**: `cargo xtask mutants kvetch-model`.
**Done when**: the committed pair carries a fingerprint and `cram --eval` still scores
it identically (the stamp changed provenance, not weights).

### Step 2: The completion loop, host-tested against a synthetic model

**Acceptance criteria**: `handle_request(buf, prefix_len, max_tokens, seed)` — the same
signature babble already serves — tokenizes the prefix, samples `max_tokens` legal
tokens, and writes the appended bytes back, truncating **only at a token boundary**
(babble's rule, and for the same reason: a mid-token cut can silently yield a
different token from the one the oracle approved). Refuses a vocab whose fingerprint
disagrees with the checkpoint's.
**RED**: babble's own suite, retargeted — `truncation_never_splits_a_token`,
`a_completion_extends_the_prefix_and_leaves_it_viable`,
`the_same_seed_serves_the_same_bytes`, plus
`a_vocab_the_checkpoint_was_not_trained_with_is_refused_rather_than_served`, built
from two *same-size* vocabs so that a token-count check would pass it and only the
fingerprint catches it.
**GREEN**: the loop, over a synthetic `Model`.
**MUTATE**: as above.
**Done when**: criteria met; the crate still has no dependency on a checkpoint file.

### Step 3: The server binary, weights embedded, on target

**Acceptance criteria**: `user/kvetch/src/bin/kvetch-drivel-server.rs` embeds
`drivel-all-30k.{kvetch,vocab}` — now committed, so unconditionally — and serves the
step-2 loop over its endpoint; `workload=stitch-drivel` boots it beside the REPL.
Booting proves the 4.2 MB image loads, maps W^X, and the model deserializes inside
`HEAP_MAX`.
**RED**: an itest asserting the server's startup span and a `kvetch.complete` span for
one request from the existing fixed-prefix client — the drivel twin of
`kvetch-babble-serves`.
**GREEN**: the binary, the `build.rs` row, the `WorkloadKind` variant + `LAYOUTS`
entry.
**Watch for**: heap headroom. `Model::decode` copies into an owned `Vec<f32>`, so the
process holds ~4.2 MB of rodata *plus* ~4.2 MB of heap against a 16 MiB cap. If it
does not fit, the fix is to borrow the weights from the mapped image rather than copy
— a `kvetch-model` API change (`Model` owns its `Vec<f32>` today), and a step of its
own rather than a scramble.
**Done when**: the scenario passes on snemu.

### Step 4: Tab at the prompt reaches drivel

**Acceptance criteria**: `stitch-drivel-completes` — the `stitch-kvetch-completes`
body against the drivel workload: Tab produces a `kvetch.complete` span on the
server's task, the suggestion is inserted at the prompt, and a **second** Tab works
(the FP-contention case).
**RED**: register the scenario; it fails until step 3's server is behind the endpoint.
**GREEN**: the `LAYOUTS` entry pairing `STITCH_REPL_DRIVEL` with the drivel server.
**Watch for**: the step budget. If a completion needs more than the 400M default, give
the scenario an explicit `budget_for` override and say so in the commit; if it needs
more than ~2B, leave it unregistered pending step 7 rather than making the gate slow.
**Done when**: green on snemu, plain and `--scramble`.

### Step 5: The completion is reproducible from the wire

**Acceptance criteria**: the host recomputes the exact bytes the guest served — same
checkpoint, same vocab, same seed — and the scenario asserts a checksum match, exactly
as `kvetch-babble-serves` does for babble. This is what makes a recorded completion
replayable rather than merely deterministic-in-the-lab.
**RED**: the checksum assertion, against a deliberately wrong seed first (it must
fail).
**GREEN**: `xtask-itest` links `kvetch-serve` and reads the committed checkpoint.
**Done when**: criteria met.

### Step 5b: Housekeeping the gate needed (done)

Getting `cargo xtask test` green surfaced three pre-existing reds that a fail-fast
run had been hiding behind each other, none of them from this work:

- `mutant_plan_tests::the_derived_plan_matches_the_previously_hardcoded_set` had been
  stale by seven crates (the whole model ladder). Re-armed, plus `kvetch-serve`. A
  tripwire nobody re-arms stops meaning "something changed".
- Three broken rustdoc intra-doc links (`illegal.rs`, `glitch/src/lib.rs`,
  `abi/src/lib.rs` — the last wanting `Self::AudioWrite` for an enum variant).
- Generated-diagram drift, from the new crate and scenarios.

### Step 6: Measure it

**Acceptance criteria**: guest instructions per completion, tokens/sec, and peak heap,
for drivel *and* babble on the same prompt — reported, not estimated. `snemu profile
--user-detail` locates where the time goes.
**Why here**: end-to-end first was the call from the desk. The number decides step 7;
it does not decide whether the feature exists.
**The prediction to check it against** (recorded now so it can be wrong in public):
~1.05M MACs per position, ~6 tokens over a ~6-token prefix with no KV cache ≈ 55M
MACs ≈ 0.2–0.5B instructions ≈ **20–50 s per Tab at snemu's 9.5M steps/s**, versus
well under a second on the VF2. If that holds, drivel-at-the-prompt is a board
feature and snemu's role is correctness, not interaction.
**Done when**: the numbers are in this file and in `notes/`.

### Step 7: Make it fast enough for the machine it runs on

**Done: the KV cache + inference-only forward** (`kvetch_model::Session`). Pulled
ahead of the profile because the token-count measurement was unambiguous on its own —
8 tokens cost 90s where 1 cost 7.9s, so per-token cost was superlinear and the cause
was structural, not a mystery worth profiling first.

`Session` keeps each layer's keys and values and runs one position per token, against
`Model::forward`'s every-position-plus-every-training-intermediate. It reconciles the
token run itself (extend on a match, rebuild on divergence), so the sampler's
backtracking cannot read a stale answer and the cache stays an optimisation rather
than a protocol. Bit-identical to re-running the prefix, asserted at every length —
which it can be, because `NaiveGemm` sums in order and the terms the batch path has
that the cached one lacks are the masked ones, whose probability is exactly zero.

Six-token REPL completion: **46.5s → 11.8s**. About 8s of that is fixed boot, so the
marginal per-token cost fell roughly 10×.

**Still open**, in the order the profile now argues for:

- **The kernel dominates a short completion.** At one token the split is 19.7%
  userspace against ~22% telemetry serialization (postcard + `wire_encode` +
  `KernelSink::emit`), 14% `prepare_switch`, 13% `memset`. Optimising the model
  further will not move a one-token gate; it moves the six-token REPL and the board.
- **Borrowed weights.** `Model::decode` copies into an owned `Vec<f32>`, so the
  process holds ~4.2 MB of rodata *plus* ~4.2 MB of heap. Borrowing from the mapped
  image would halve resident memory and is what would let the 64 MiB machine come
  back down.
- **snemu's JIT ends a block at every FP op.** `block.rs::compile_op` lowers no FP
  family (and no `MULDIV`), so a matmul inner loop compiles to two-instruction blocks
  and the block machinery is pure overhead. This is emulator wall-clock only — it does
  nothing for the VF2 — but it would pay for every float-heavy guest: the audio path
  and on-target Stitch floats as well as drivel.
**Done when**: a Tab under snemu is bearable, or we have decided in writing that it
is a board-only feature and the snemu scenario exists for correctness alone.

## Known unknowns

- **Speed under snemu** — the whole of steps 6 and 7. Deferred on purpose; the risk it
  carries is that step 4's scenario may not be gate-able at default budget.
- **The 4.2 MB ELF.** Loader and W^X planning are 7× outside their exercised range.
- **Heap headroom**, per step 3: ~8.4 MB of a 16 MiB cap before activations.
- **Which checkpoint is canonical.** `drivel-all-30k` today, and now committed, so
  "canonical" is a fact in git rather than a habit on one machine. When a better rung
  is trained, both halves move in one commit and the fingerprint refuses the
  half-updated state. Each replacement costs another 4.2 MB of history, which is the
  brake that keeps this deliberate.

## Pre-PR quality gate

1. Mutation testing on `kvetch-serve` (steps 1 and 2).
2. `cargo xtask clippy` — host + riscv.
3. `cargo xtask test && cargo xtask itest && cargo xtask itest --scramble`.
4. Confirm the committed pair's fingerprints agree — and that swapping in another
   same-size vocab is refused, since that is the check's whole reason to exist.
5. `cargo xtask links` if any doc moved.

---
*On completion, `git mv` this file to `plans/legacy/` (house override of the planning
skill's "delete when complete").*
