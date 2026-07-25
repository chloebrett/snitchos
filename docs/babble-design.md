# babble: the weight-free model (design)

**Status:** 📐 **DESIGN — the meta-tracer.** babble is rung 0 of the
[generative ladder](generative-ladder.md): a *policy with no weights* —
sample the next token uniformly (well, biasedly) from whatever the
continuation oracle says is legal. Its outputs are deliberately worthless;
its **deliverables are interfaces**. Building babble forces four contracts
into existence that every later rung inherits: the continuation oracle's
API, the sampler's bias tables (shared with Tier-0 corpus generation), the
kvetch completion-endpoint protocol v0, and the eval report's chance-level
floor. A tracer bullet for the tracer bullet (quip) to follow. TDD plan:
[../plans/babble.md](../plans/babble.md).

Related: [llm-design.md](llm-design.md) (continuation oracle rationale,
kvetch service, help/completion architecture),
[generative-ladder.md](generative-ladder.md) (babble's two hats, eval
floor), [language-design.md](language-design.md) (Stitch).

---

## What babble is (and the one thing it isn't)

```
loop {
    let legal = oracle.valid_next(&prefix);        // the Stitch parser speaks
    let tok   = sampler.pick(legal, &bias, &mut rng); // biased, seeded, deterministic
    prefix.push(render(tok));                       // lexeme synthesis for terminals
}
```

Three properties, all load-bearing:

- **Every output is syntactically valid Stitch by construction.** babble
  cannot emit garbage — only meaninglessness. (This is the property the
  whole constrained-decoding stack rests on; babble is its first proof.)
- **Deterministic given a seed.** Same seed, same bias tables, same
  grammar → same program. This is what makes babble itest-able and the
  eval floor reproducible.
- **No ML vocabulary.** babble samples *Stitch lexical tokens* (the
  parser's alphabet), not BPE tokens. This deliberately breaks the
  dependency on the tokenizer: babble ships before any vocab exists. The
  BPE mask-lifting layer is a *quip-time* component with its own tests —
  the one interface babble does **not** exercise, stated up front.

## Component 1: the continuation oracle (in the `stitch` crate)

The load-bearing new API — pure, host-tested, no_std-compatible:

```
valid_next(source: &str, pos: usize) -> TokenSet
```

**Mechanism — replay with a sentinel** (per
[llm-design.md](llm-design.md)): lex+parse the prefix with a sentinel
token appended; instrument every `expect`/`peek` decision point so that
*hitting the sentinel* records the token kinds the parser would have
accepted there, then unwinds and tries remaining alternatives. For a
mostly-LL(1) grammar this collects the exact set. O(prefix) per query —
fine for babble (one query per emitted token) and for editor keystrokes
later (snapshot-caching is a stim-time optimization, out of scope here).

Design decisions:

- **TokenSet is kinds + constraints, not just kinds.** "Identifier is
  legal here" sometimes carries a constraint (e.g. after `use `, a module
  name). v0: kinds only, one variant carrying "any identifier"; constraint
  refinement (type-directed narrowing) is explicitly deferred.
- **The lexer layer rides along**: for each legal kind, its lexeme
  grammar. The maximal-munch wrinkle (the call-paren gotcha) means char
  meaning can depend on lexer lookahead — one dedicated test, per the
  standing note.
- **This API is the diagnostics API.** `expected one of {…}` error
  messages become a *view* of `valid_next`. Land the oracle and the parser's
  error messages improve as a side effect — the first of the four consumers
  (mask, affordances, diagnostics, highlighting) arrives on day one.
- Prerequisite already held: the stitch parser is no_std+alloc and builds
  for riscv64 (established during stitch-on-the-metal) — the oracle must
  keep that property, since the serving hat runs it on-target.

## Component 2: the sampler and its bias tables

`babble(oracle, bias, seed) -> impl Iterator<Item = u8>` — a seeded walk.
Pure uniform sampling either terminates in three tokens or nests to the
heat death of the universe, so the bias tables are not optional garnish;
they're the difference between noise and *shaped* noise:

- **Per-kind weights**, CSmith-style probability tables.
- **Depth damping**: weights on nesting constructs decay with current
  depth (drives termination; makes expected program length a tunable).
- **Terminal synthesis**: when the sampled kind is an identifier/literal,
  a generator produces the lexeme — wordlist-driven (the same must-use
  wordlists the corpus recipes use) with a seeded fallback.
- **Shape-statistics hook**: table values are data, not code — the same
  format the Tier-0 corpus sampler will fill from *measured* real-corpus
  statistics (arity/nesting/match-arm distributions). babble v0 ships
  hand-tuned defaults; the measurement pipeline replaces them later
  without touching the sampler.

Property tests (the sampler's contract): every emission keeps the prefix
viable (re-run the oracle and check membership — the sampler validates
itself against its own oracle); seeded determinism; termination within a
bound for default tables; emitted programs parse end-to-end.

**The two hats, explicitly:** batch hat = a host CLI (`babble gen --count N
--seed S`) feeding the validator/metrics harness — this *is* Tier-0 corpus
generation and the first producer of per-batch reports (production
coverage of pure grammar-walks is the coverage ceiling every recipe is
measured against). Serving hat = the same crate linked into the on-target
server below.

## Component 3: kvetch v0 — the serving hat

The first kvetch is a userspace Stitch-parser-plus-sampler behind an IPC
endpoint. No weights, no FP, no blobs — which is exactly why it can land
before any of the runner's fixed-point work:

- **Process shape**: `kvetch` userspace bin (SPAWNABLE registry), serve
  loop modeled on the FS server: `Receive` → compute → `Reply`. Holds
  telemetry + span caps; the completion endpoint arrives per the standard
  handle discipline (`delegated_handle(0)` / `run_ipc` handle 2).
- **Protocol v0 — stateless, versioned, small**:
  - request: `{ version, seed, max_bytes, prefix_len, prefix bytes… }`
  - reply: `{ version, status, completion bytes… }`
  - The versioned-buffer protocol (KV chains, edit deltas) is a *later
    revision behind the same endpoint* — v0's `version` field is the only
    forward-compatibility commitment made now. Stateless-with-prefix is
    wrong for real completion latency and right for proving the path.
- **Entropy discipline** (see
  [randomness-and-entropy.md](randomness-and-entropy.md), sampling section):
  the seed is always explicit, and **time never enters seed derivation** —
  clock-derived seeds would promote engine clock skew into content
  divergence, poisoning `snemu diff`. All sampling randomness derives from a
  per-boot entropy root: `request_seed = hash(boot_seed, request_counter)`.
  `boot_seed` enters once at boot (bootarg `seed=…`, same mechanism as
  `workload=`; hardware RNG later) and is emitted as a frame immediately;
  the counter is uniqueness-category, not randomness. itests pin the client
  seed outright. Same boot_seed + same request sequence → byte-identical
  output on snemu, QEMU, and the board; every generation replayable from
  telemetry — deterministic *given its own trace*.
- **Telemetry from day one**: a completion span per request;
  `kvetch.requests_total`, `kvetch.tokens_emitted_total`, and
  `kvetch.oracle_time` metrics via `RegisterMetric` (well under the
  16/process quota; register once at startup).
- **itest scenario `kvetch-babble-serves`**: boot with a workload that
  spawns kvetch + a client; client sends a fixed-seed request with a fixed
  prefix; assert (a) a reply arrives, (b) the completion appended to the
  prefix parses on the host side of the harness, (c) the completion is
  byte-identical to the host-side sampler's output for the same
  seed/tables (determinism across host and target — the strong version of
  the assertion, and snemu makes it cheap), (d) heartbeat survives.

## Component 4: the eval floor

babble's scores are the zero line every trained rung is measured against.
The eval harness (built here, inherited by quip) records, per model:
unconstrained-parse% (babble: 100% *by construction* — which is exactly
why the metric is only meaningful unmasked, i.e. from quip on), held-out
FIM test-pass% (babble: ~0), idiom-match vs the gold set (babble: ~0),
and length/shape distribution (babble: whatever the tables say — the
"no learning happened" reference distribution).

## Increments (each lands green before the next starts)

1. **Oracle** in `stitch` — pure, host-tested, replay-with-sentinel. The
   test surface is enumerable ("after `use M.{`, exactly {ident, `}`}").
   Diagnostics improvement falls out.
2. **Sampler + bias tables** — pure, seeded; property tests above.
3. **Batch CLI + harness hookup** — first per-batch report, first Tier-0
   corpus tokens, shape-stats hook stubbed with hand-tuned defaults.
4. **kvetch v0 on-target** — process, endpoint, protocol v0, telemetry,
   the itest. (The only increment touching kernel-adjacent surface, and
   it's all existing machinery: SPAWNABLE, run_ipc, RegisterMetric.)
5. *(deferred to quip)* BPE mask-lifting over the oracle, once a vocab
   exists.

## Open questions

- Crate placement: oracle in `stitch` (it *is* parser knowledge); does the
  sampler live in `stitch` too or in a small `babble` crate that both the
  host CLI and the kvetch bin consume? (Lean: separate crate — the bias
  tables and terminal wordlists are not parser business.)
- Oracle return for *incomplete tokens* (cursor mid-identifier): v0 can
  sidestep (babble only ever appends at token boundaries), but the API
  shape should not preclude it — stim will need it.
- Protocol v0: does the reply carry the oracle's valid-set for the final
  position (useful for client-side affordances later), or stay minimal?
- Where do hand-tuned default bias tables live — code, or a checked-in
  data file the measurement pipeline later overwrites?
