# Making drivel-at-the-prompt fast on the VF2 — candidate levers

**Status:** 📐 ideas, unmeasured. Written 2026-08-06 by reading the serving path end
to end. Nothing here has been benchmarked on the board, and §0 argues that should
change before any of §2–§4 is attempted.

Related: [../plans/legacy/kvetch-drivel-on-target.md](../plans/legacy/kvetch-drivel-on-target.md)
(what shipped), [../docs/debt-register.md](../docs/debt-register.md) (#19),
[../docs/llm-design.md](../docs/llm-design.md) (the int8 plan),
[../docs/speculative-decoding-design.md](../docs/speculative-decoding-design.md).

---

## 0. Measure first — and the cheap place to do it is the host

**There is no on-board tok/s number anywhere in the tree.** Every measurement we
have is snemu wall-clock (46.5s → 11.8s for six tokens, of which ~8s is boot), and
the only per-function profile is the *one-token* one, where the kernel dominates by
construction (19.7% userspace vs ~22% telemetry serialization, 14% `prepare_switch`,
13% `memset`). That profile does not describe a six-token Tab, and it certainly does
not describe the board.

The stock-take names "the long-completion profile" as the #1 unpulled lead and says
it wants an idle machine. **It doesn't — not for the question that matters.** The
question is *"inside userspace, is the time going to the transformer or to the
grammar oracle?"*, and that split is a property of `kvetch_serve`, which is pure,
`no_std`, and already host-tested:

```rust
// kvetch-serve/src/serve.rs:54 — no syscalls, no kernel, no emulator
server.handle_request(&mut buf, prefix_len, max_tokens, seed)
```

So: a host bench that calls `handle_request` with a realistic REPL prefix and
`max_tokens = 6` (what `RuntimePlatform` asks for — `stitch/src/platform.rs:473`),
with a counter around `Session::logits_for` and one around `extends_legally`,
answers it in seconds. The absolute numbers won't transfer to the U74; **the ratio
will**, because both halves are scalar `f32`/pointer-chasing work with no vendor
acceleration on either machine.

This is the project's own repeated lesson — post 80's *"when a control fails to
discriminate, measure the discrimination somewhere cheap"*, and
[[measure-dont-estimate]]. Note that `snemu profile --user-detail` will *not* answer
it cheaply: it emits raw PCs (`[user:0x100053ac]`) with no symbolisation, so you
objdump a 4.5 MB stripped ELF by hand.

Everything below is ranked by my guess at (win × cheapness). §0 exists because I
expect at least one of those guesses to be wrong by an order of magnitude — that is
what happened to the 0.2–0.5B instruction prediction, and to every perf estimate in
post 73.

---

## 1. The board is running unoptimised code, which is not a model problem at all

### 1a. Every board image is a debug kernel **and a debug userspace** — debt #19

`image()` calls `qemu::build_kernel`, which is `build_kernel_profiled(features,
OptLevel::Low)` (`xtask-qemu/src/lib.rs:146-148`). `Low` passes no `--release`, so
`kernel/build.rs:152` sees `PROFILE=debug` and builds the embedded userspace with no
`--release` either. **The drivel server on the board is compiled at opt-level 0.**

The debt register frames this as "a transformer forward pass wrapped in debug-build
kernel overhead". That undersells it: the forward pass *itself* is opt-0. Every
`a[row * k + inner]` in `NaiveGemm` is a bounds-checked load with no register
allocation, no unrolling, no FMA contraction. For a scalar dot-product loop that is
routinely 10–30×.

The fix is one flag threaded to `build_kernel_profiled`, mirroring the ladder `itest`
already has, sitting next to `image_features`. The register's own caveat stands and
should be honoured: release `vf2` images are the regime where both the `tp`-truncation
and the SBI `a1`-clobber bugs lived, the latter hidden *precisely because* board
images are debug. Land it deliberately, with a boot on the board immediately after.

**This is the single highest-leverage change in this document and it touches no model
code.** It is also the thing that makes every other number in §2–§4 meaningful, since
measuring an opt-0 build tells you about LLVM's opt-0 output, not about your design.

### 1b. `+zba,+zbb` are on the board and off in the build

The VF2's own ISA string, read off the live board, is
`rv64imafdc` + **`zba zbb`** + `zicntr…` (`plans/visionfive2-port.md`). The stock
`riscv64gc` target enables neither, and the only rustflags we set are
`-C code-model=medium` (`.cargo/config.toml:9-10`).

`-C target-feature=+zba,+zbb` in that stanza is a one-line change. Zba's
`sh1add`/`sh2add`/`sh3add` collapse the index arithmetic that dominates a
bounds-checked scalar kernel; Zbb's `min`/`max`/`clz` help the lexer and the softmax
reduction. Modest — single-digit percent, probably — but free, and safe: QEMU `virt`
advertises `zba zbb zbc zbs`, a strict superset, so the emulated path is unaffected.

Caveat: `kernel/build.rs:194` scrubs `RUSTFLAGS`/`CARGO_ENCODED_RUSTFLAGS` from the
nested userspace build, so this **must** go in `.cargo/config.toml` and cannot be
injected by env var. That is the right place anyway.

---

## 2. The legality oracle, not the transformer, is my prime suspect

This is where I'd put money before measuring, and it is worth stating why so §0 can
refute it.

### 2a. `viable()` builds 118 ASTs to answer a boolean

```rust
// kvetch-serve/src/serve.rs:135-140
fn viable(text: &str) -> bool {
    !valid_next_in(text, text.len(), Entry::Program)
        .union(valid_next_in(text, text.len(), Entry::Expr))
        .is_empty()
}
```

`valid_next_in` (`stitch/src/oracle.rs:577`) has **no early exit** — it folds all 59
token classes into a `TokenSet`. Each class costs one `Probes::with` (a deep clone of
the whole prefix token vector, including a `String` per `Ident`) plus one full
`parse_program_tokens` that builds an AST and throws it away
(`oracle.rs:498-517`, `:439-445`).

So one legality verdict = **2 lexes + 118 parses + ~118 token-vector deep clones**.
And `draw` asks for up to `MAX_REFUSALS + 1 = 17` verdicts per token
(`kvetch-serve/src/sample.rs:39`). At six tokens per Tab that is up to ~12,000 parses
of a growing prefix, each allocating on a `talc` heap inside a 16 MiB process.

The oracle's own doc comment already argues the fix, one function above:

> *"[`valid_next`] answers it for all 58 classes, which is what a decoder mask or a
> diagnostic needs — but a **sampler** needs only one viable class, and asking one at
> a time until it finds one is an order of magnitude cheaper (each query is a parse)."*
> — `stitch/src/oracle.rs:565-568`

`viable` is a sampler and it takes the diagnostic path. The replacement is an
`oracle::admits_any(prefix, entry) -> bool` that shares `Probes` and returns on the
first admitting class, with `viable` short-circuiting across entries:

```rust
admits_any(text, Entry::Program) || admits_any(text, Entry::Expr)
```

Semantically identical — `(A ∪ B).is_empty() ⟺ A.is_empty() && B.is_empty()` — so the
byte-exact itest should be unmoved. Expected win: large but order-dependent. `ALL` is
in discriminant order, which the REPL work already found "front-loads literals (good
for openers) and keyword operators (bad for continuations)"
([../plans/repl-completion.md](../plans/repl-completion.md)). Two cheap refinements:

- **Ask the drawn token's own class first.** `extends_legally` knows which byte
  sequence it is testing; if that piece lexes to a class, probe it before the walk.
  A legal candidate then costs *one* parse instead of an average walk.
- Reorder `ALL` by measured frequency, or keep a per-position memo.

### 2b. `extends_legally` copies the whole prefix per candidate

```rust
// kvetch-serve/src/serve.rs:128-130
let mut extended = String::from(text);
extended.push_str(piece);
viable(&extended)
```

One full prefix copy per candidate, up to 17 per token. Trivial to hoist: keep one
`String`, `push_str` the piece, `truncate` back on refusal. Small next to 2a, free to
take while you are in the file.

### 2c. The lex is repeated per candidate, and it need not be

Every candidate at a position shares the same *prefix*; only the appended piece
differs. `Probes::new` lexes the prefix (`oracle.rs:486`) and is rebuilt inside every
`valid_next_in` call, so a position pays ~34 lexes of the prefix instead of one. If
2a lands, `Probes` is already the natural thing to hoist out of the refusal loop.

### 2d. The deeper shape: cost is quadratic in completion length

Every probe parses the **whole** prefix, so per-token cost grows with the prefix and
a completion is `O(n²)`. [../plans/drivel.md](../plans/drivel.md) already measured
this on the host at corpus scale — *"~138 s for 8,318 decisions — ~17 ms each … it
will not scale"* — and names the fix: incremental parsing, or scoring per top-level
item rather than per file. At a 256-byte REPL buffer this is survivable; it is the
reason a *long* completion (stim, ghost text) will not be. Worth knowing about, not
worth doing before 2a.

---

### 2e. The tokenizer allocates ~90,000 `Vec`s per Tab, before a token is generated

Not per token — **per request**, and therefore invisible to any per-token reasoning,
which is why nobody has named it.

```rust
// kvetch-vocab/src/lib.rs:245-255
fn encode_chunk(&self, chunk: &str) -> Vec<TokenId> {
    self.merges.iter().enumerate()
        .fold(initial, |ids, (index, &pair)| collapse_pair(&ids, pair, merged_id))
}
```

The fold runs over **all 1792 merges** for **every chunk**, and `collapse_pair`
returns a fresh `Vec` each time. A 200-byte prefix pre-tokenizes to ~50 chunks, so
`Server::handle_request`'s single `self.vocab.encode(prefix)` (`serve.rs:70`) costs
~90,000 heap allocations and ~90,000 vector copies on a `talc` heap. Once per Tab.

**The merge-order semantics are wire law and must not change** — the doc comment at
`:236-237` is explicit that applying merges in order, rather than repeatedly taking
the highest-priority present pair, is what makes the encoding a pure function of the
merge list. But the *implementation* is free:

- `collapse_pair` in place, two-pointer compaction into one reused buffer: 1792
  allocations → 0, same output.
- Skip merges that cannot apply. Keep a presence bitmap over ids (2048 bits = 256
  bytes) and only scan when both halves of the pair are present. Most merges apply to
  no chunk; the merge *order* is untouched, so the encoding is identical.

Both are local to `kvetch-vocab`, both are covered by the existing round-trip tests,
and neither touches the frozen vocab artifact.

---

## 3. The forward pass

drivel is `d_model` 128, 4 layers, 4 heads, `ffn` 512, vocab 2048 = **1,049,728
params / 4.2 MB f32**. With the KV cache a decode step is ~2.1 MFLOP and reads each
weight once. At the board's measured 1.5–3 GB/s that is a **~1.4–2.8 ms/token
bandwidth floor, i.e. ~350–700 tok/s** — which is the number to compare any
measurement against, and is far above anything we are seeing.

### 3a. The on-target GEMM walks the weight matrix **down columns**

`NaiveGemm` (`kvetch-model/src/lib.rs:211-241`) computes each output element as an
independent dot product. Every projection in `Model::step` passes
`transpose_b: false` (`:581`), so the inner loop indexes `b[inner * n + column]` —
striding the weight matrix by `n` floats per step, for a fixed column.

Each 64-byte cache line therefore yields **one useful float**. `w1`/`w2` are 256 KB
each and get re-streamed once per output column: ~16× more L1↔L2 traffic than
necessary, on a core with a 32 KB L1. Across 4 layers that is tens of MB of avoidable
traffic per token.

**The fix is already written, in `cram`.** `blocked_band` (`cram/src/lib.rs:53-82`)
accumulates into the output row over `k`, which its own doc calls *"the whole win over
the naive triple loop"*. The body is `core`-only — the `std` is in the threading
wrapper above it. Lift it into `kvetch-model` as a `no_std` `RowGemm` and have both
`BlockedGemm` and the serving path use it, so there is one kernel rather than two.

**It should be bit-identical at `m = 1`, which is the constraint that matters here.**
Reordering the *traversal* does not reorder the *accumulation*: each output element is
still summed over `k` ascending into a zero-initialised slot, so the byte-exact
`stitch-drivel-completes` assertion and
`generating_with_a_cache_is_bit_identical_to_re_running_the_prefix` both still hold.
One exception to watch: drop `blocked_band`'s `if left == 0.0 { continue }` skip
(`cram/src/lib.rs:66-68`) — it is a no-op for finite values but not for `inf`/`NaN`
and not for signed zero, and "not provably identical" is not worth the branch here.

### 3b. `rope_one` computes 512 transcendental triples per token where 16 would do

```rust
// kvetch-model/src/lib.rs:898-909
fn rope_one(row: &mut [f32], head_dim: usize, position: usize) {
    for head in row.chunks_mut(head_dim) {          // 4 heads
        for pair in 0..head_dim / 2 {               // 16 pairs
            let angle = rope_angle(position, pair, head_dim);   // powf
            let (sin, cos) = (libm::sinf(angle), libm::cosf(angle));
```

The angle is a function of `(position, pair)` **only** — not head, not Q-vs-K, not
layer. But the computation sits inside the head loop, and `rope_one` is called twice
per layer (`:593-594`) across 4 layers. So drivel pays **512 `powf` + 512 `sinf` +
512 `cosf` per token for 16 distinct angles: 32× redundant.**

The doc comment defends the missing table — *"a table amortises `powf` across
positions, and there is one position here"* — and that is exactly the miss. It does
not need to amortise across positions; it needs to amortise across the 4 heads, the
Q/K pair, and the 4 layers, all of which are inside the loop.

**This is the same bug the training path already had and already fixed.** Post 73
records `RotationTable` as the single largest win of the whole training-throughput
sweep — 55,945 tok/s from 37,055, *"a table lookup beat every matmul optimization,
which is not where anyone looks first"* — and `RotationTable::new`
(`kvetch-model/src/lib.rs:837-869`) contains the exact hoisting logic. The generation
path never got it.

Bit-identity is preserved as long as the hoisted values come from the *same
expression in the same order* (`rope_angle`, then `sinf`, then `cosf`) — which is
already the constraint `rope_one`'s doc comment is written to satisfy, and is what
`generating_with_a_cache_is_bit_identical_to_re_running_the_prefix` will confirm.

Cheapest version: hoist the 16 `(sin, cos)` pairs to the top of `rope_one` (4×
saved). Full version: compute them once per `Model::step` and thread them through
(32× saved). Alongside it, `attention_scale` (`:1099-1101`) does a `sqrtf` per head
per layer — 16 per token for a value that depends only on `head_dim`.

**Cheap, mechanical, bit-identical, and it is the lever the project has already
learned once.** If I had to pick one model-side change without measuring, it's this
one rather than the GEMM.

### 3c. `gather_head` copies the whole KV history, per head, per layer, per token

```rust
// kvetch-model/src/lib.rs:1049-1062
(0..positions).flat_map(|position| data[position * d_model + offset..][..head_dim]…).collect()
```

`attend_last` calls it twice per head (`:935-936`), so drivel pays **32 heap
allocations per token**, each copying `positions × head_dim` floats — and each grows
linearly with the prefix. For a 100-token prefix that is ~400 KB of memcpy per token,
on a `talc` heap.

The cache is stored position-major (`positions × d_model`) because that is what
`Model::forward`'s batch path wants. Storing it **head-major** — one `Vec` per
(layer, head), positions contiguous — turns both gathers into slices and deletes the
copies outright. It changes no arithmetic, so bit-identity is preserved by
construction.

### 3d. `Model::step` makes ~116 heap allocations per token

Counted across `lib.rs:560-628`: 2 per `rms_norm` (it always allocates and returns
the inverse-RMS the inference path never reads), one per `project`, 4 per head in
`attend_last` (`gather_head` ×2, `scores`, `attended`), the `activated` collect, and
the 8 KB `logits` vector — ~28 per layer × 4, plus the tail. All on the `talc` heap,
in a process already carrying 8.4 MB.

Give `Session` a scratch arena sized from `ModelConfig` at first use and reused
across steps. Mechanical, and it composes with 3c (which deletes the 32 largest of
them outright).

### 3e. `weighted_pick` re-sums the whole vocab on every refusal

`sample.rs:99` sums all 2048 weights, and `draw` calls it once per attempt — up to
17× per token — when a struck candidate changes the total by exactly its own weight.
A running total decremented on each strike is exact and turns 17 full scans into one.
The inverse-CDF walk itself still has to run, but the sum is pure waste.

Related: `weights_from_logits` (`sample.rs:84-87`) exponentiates the full 2048-entry
vocab every token — ~2048 softfloat `expf` calls. Genuinely needed for a proportional
draw, so not free to delete; a top-k prefilter would change the sampled output, which
the byte-exact scenario would (correctly) reject. That one is a *decision*, not a win.

### 3f. Borrowed weights (already on the loose-ends list)

`Model::decode` copies the embedded checkpoint into an owned `Vec<f32>`, so the
process holds ~4.2 MB of rodata *plus* ~4.2 MB of heap. This is the named lever that
would let the 64 MiB machine come back to 16. It costs a 4.2 MB copy at startup and
halves resident memory; it does **not** move steady-state tok/s. Worth doing, wrong
thing to do first.

---

## 4. The designed future, and one tension in it

### 4a. Forced tokens are free and exact — but they fight §2a

*"8.3% of decode steps cost zero forward passes"*
([../docs/speculative-decoding-design.md](../docs/speculative-decoding-design.md)),
measured on real Stitch. No second model, no approximation.

But detecting `|legal| = 1` needs the *full* legal set, which is exactly what §2a
stops computing. The resolution is that "is there exactly one" is still an early-exit
walk — stop at the **second** admitting class rather than the first. So `admits_any`
generalises to `at_most_one(prefix)`, and both optimisations compose. Worth designing
that way from the start rather than retrofitting.

Note also that these are two different token alphabets: the oracle speaks Stitch
*token classes*, the sampler speaks BPE *pieces*. A forced class is not a forced piece
(`oracle::has_one_spelling` already draws that line for the REPL). The 8.3% is an
upper bound on what this buys at the byte level.

### 4b. babble-drafting: measured, and it does not pay here

`n_max = ⌊1/(c−1)⌋`; at ballad on the VF2 `c ≈ 1.5` → `n_max = 2`, and `n = 1` is
already covered by forced tokens, leaving a 5% slice at exactly break-even.
**Recorded negative result — do not re-derive it.** It inverts in bandwidth-bound
regimes, which is where *drivel* sits (1M params, ~4 MB), so it may be worth
re-checking at this rung specifically — but only after §1 and §2, since `c` should
fall out of the runner's own telemetry rather than a microbenchmark.

### 4c. Multi-hart matmul drags a kernel change with it

Four U74s, and `blocked_band` is already shaped for row-banding. But at `m = 1` there
are no rows to band — you would partition over output columns `n` instead. The real
obstacle is upstream: this needs a second task in the server process, and **FP
ownership is per-*process* (`Process::fp_enabled`) while the registers are
per-*task***. That gap is recorded in the stock-take as "identical only while each
process has one task — true today, verified nowhere", and its stated trigger is
exactly this. So multi-hart inference is a kernel milestone wearing a model hat.
~3–4× available; not cheap.

### 4d. int8 is bigger than the plan thinks

The stated target is ballad-class, ~4× less bandwidth, no FP at all. But
[../plans/drivel.md](../plans/drivel.md)'s own hedge — *"keep the forward pass generic
over a `Weights` accessor rather than indexing `&[f32]` directly, so int8 arrives as a
second impl rather than a rewrite"* — **was not honoured**. There is no `Weights`
trait; `Gemm::sgemm` takes `&[f32]`, `Model::weights()` hands out `&[f32]`, and the
checkpoint has no dtype field (it is `f32::from_le_bytes` over the payload,
`kvetch-model/src/lib.rs:538-541`). So int8 today means touching the trait, the
forward pass, and the checkpoint format at once. The checkpoint half is cheap — v3
appending a dtype follows the v2-appends-a-fingerprint precedent — but the accessor
debt is real and should be paid before, not during.

Also worth noting for drivel specifically: at 1M params the model is 4.2 MB and the
board has 4 GiB. int8 buys bandwidth, not capacity, and the bandwidth ceiling
(~350–700 tok/s) is nowhere near binding today. **int8 is a ballad lever, not a
drivel one.**

### 4e. snemu's block JIT lowers no FP

Emulator wall-clock only — it does nothing for the board. It would pay for the audio
path and on-target Stitch floats as much as for drivel. Listed here only so it is not
confused with a board lever.

---

## What I would actually do, in order

1. **The host split bench** (§0). An afternoon at most. It decides whether §2 or §3
   is the real work, and it should time four buckets, not two: `vocab.encode`
   (once per request), `Session::logits_for`, `extends_legally`, and the rest.
   Without it, everything below is a guess.
2. **`--opt` on `cargo xtask image`** (§1a). One flag, expect the largest single
   factor, and it makes every subsequent measurement mean something. Boot the board
   straight after — this is the regime that hid the `tp` truncation and the SBI
   `a1` clobber.
3. **`rope_one`'s 32× transcendental redundancy** (§3b). Smallest diff in this
   document, bit-identical, and it is the same lever that was the single largest win
   of the training-throughput sweep.
4. **Short-circuit `viable`** (§2a), designed as `at_most_one` so §4a composes, with
   the free 2b/2c cleanups alongside it.
5. **In-place `collapse_pair`** (§2e). Deletes ~90,000 allocations per Tab without
   touching merge order, so the frozen vocab is untouched.
6. **Row-order GEMM in `kvetch-model`** (§3a), lifted from `cram::blocked_band`, with
   the bit-identity assertion re-run as the gate.
7. Re-measure, then choose between §3c (head-major KV) and §4a (forced tokens).

Steps 2–6 are independent and each is small. Every one of them is bit-identical
except §1a, so `stitch-drivel-completes` stays the gate throughout — which is the
unusual luxury here: the byte-exact scenario means a performance change that alters
behaviour cannot land quietly.

My guess is they compound to a large factor. My guess is exactly what §0 exists to
check — and note that on this project the estimates have been wrong in both
directions, repeatedly.

## One thing to fix while passing

`xtask-itest/src/itest/snemu_audit.rs:1376-1382` still budgets the drivel scenarios at
8B guest instructions and cites "with no KV cache — measured at 4-8B guest instructions
(~90s under snemu)". `Session` landed; `kvetch-serve/src/model.rs:55` uses it. The
budget was never revised and is plausibly ~10× oversized — which matters because an
oversized budget is what turns a hang into a fifteen-minute gate rather than a fast
failure.
