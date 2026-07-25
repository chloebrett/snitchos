# babble — the weight-free model (TDD plan)

**Status:** ✅ **COMPLETE for its purpose — increments 1, 2, 4, 4b, 5, 6, 7, 8,
10, 11, 12, 13 done.** babble generates valid Stitch, serves completions over a
capability-mediated IPC endpoint from a userspace process, and its output is
byte-identical on host and target — the four contracts the ladder inherits
(oracle API, bias tables, endpoint protocol, eval floor) all exist and are
gated.
(9 **superseded** — `cram-corpus` does Tier-0 generation better than the planned
`babble gen`: a cached corpus with a host-tested `Manifest` deciding when the
cache is stale, rather than a naive file-writer; 3 deferred: it is
the char-level view, which stim needs and babble does not — babble appends
whole space-separated tokens, so maximal munch cannot bite it). The oracle
(`stitch/src/oracle.rs`) answers `valid_next` across the grammar; 9 tests
green. **Mechanism changed from the design doc's "instrument `expect`/`peek`"
to *trial-by-append*** — append a class's representative lexeme and read
*where* the parser fails: an error at the appended token means rejected
(dead), an error beyond it (or none) means consumed-and-wants-more (viable).
The real parser answers every query, so the oracle cannot drift from the
grammar — no second copy of grammar knowledge exists. Cost is one parse per
class (58) per query; fine for babble, and the deferred snapshot-caching is
the stim-time fix. **Increment 4 shipped as `ParseError::expected(src)` +
`render`**: diagnostics now end with `expected one of: …`, sourced from the
oracle rather than a hand-maintained list, so mask and message can never
disagree. No recursion risk — the oracle parses, so it is consulted on a
*returned* error, never from inside a parse. **Increment 4b** closed the
entry-point gap: `oracle::Entry{Program,Expr}` + `valid_next_in`, and
`ParseError` records which entry produced it (tagged at the two entry
points), so REPL/expression errors get expression continuations rather than
"nothing is legal". Diagnostics list at most `SHOWN_CONTINUATIONS` (8) then
`… (N total)` — mid-expression the legal set is two dozen classes. Also
fixed two paths that formatted `error.message` and so dropped the caret
*and* the continuations: module loading and `Repl::load_source` — a parse
error in a *file* had been strictly less helpful than the same error in a
single-file program.

**Increment 5 (sampler) done** — new `babble` crate: seeded xorshift64\*,
`walk(seed)` recording a `Step` trace, `generate(seed) -> String`. Three tests
green in ~1s. Two findings: (a) **lazy sampling** — shuffle the classes and
take the first the oracle admits, which is *exactly* a uniform draw over the
legal set but costs ~`58/|legal|` queries instead of 58; this plus a 200-token
cap took the suite from 50s to 1.06s, and motivated exposing
`oracle::admits_next` (single-class) beside `valid_next` (full set), with a
test pinning that the two agree so they cannot drift; (b) walks currently run
to the cap rather than choosing `Eof` — expected, and exactly what increment
6's depth damping fixes, so babble's programs do not yet parse whole.

**Increments 6 + 8 (tables, termination, the parse property) done** — 7 tests,
1.2s. Three findings, each from measurement rather than guesswork:

1. **Weights alone cannot make a grammar walk terminate.** Damping every
   obligation-creating class also damps the tokens that *pay* an existing debt
   (the `{` a pending `match` is waiting for), so the walk enters constructs it
   is then discouraged from finishing. Instrumenting showed `Eof` was legal at
   only **2–7 of 200 steps** under both the uniform and the damped walk — the
   pressure never got a turn. Fix: **ask the oracle when to stop.** Past
   `wind_down`, end at the first point the program is complete; if the cap
   arrives still owing a construct, rewind to the last complete point. A
   babbled program is therefore always a whole program, never a truncated
   prefix. Weights still shape *what* is generated — they just no longer have
   to shape when it ends.
2. **A per-class floor of 1 silently flattens the policy.** With ~25 operator
   classes pinned at weight 1, they always outvoted `Eof`. Fixed by scaling
   weights (`WEIGHT_SCALE`) so the floor is negligible, and capping pressure
   (`PRESSURE_CAP`) so the divisions keep resolving instead of every class
   bottoming out at 1 exactly when the walk most needs to finish.
3. **TOML tables were dropped** — the plan called for
   `babble/tables/default.toml` via `include_str!`, but the `toml` crate is
   std-only and babble is `no_std`. Tables are a Rust `const` (`Tables::DEFAULT`),
   still data rather than logic, still regenerable by the future
   shape-statistics pipeline.

Increment 8's property (`every_babbled_program_parses`, 64 seeds) fell out of
(1) rather than needing its own work: babble stops only where the parser says
the program is whole, so parseability is structural.

**Completeness is a policy, not a property** (design correction, 2026-07-25).
The whole-program guarantee above is right for the *corpus* hat and wrong for
the *serving* hat: a completion at a cursor is a legal **fragment**, and
demanding a whole program would mean answering `greet(name) {` with the rest of
the file. So `Stop::{WholeProgram, AfterTokens}` parameterises the walk
(`walk_from`, plus `complete(prefix, seed, max_tokens)` for the serving path).
What survives in both modes is the property that actually matters: every
emitted token was legal, so the buffer is always left **viable** — still
extendable to something valid. That is strictly stronger than what mainstream
autocomplete offers, and it is what increment 12's `handle_request` will
serve.

**Increment 7 (terminal synthesis) done** — payload classes draw from
wordlist/literal stock instead of the oracle's probe lexeme, so output reads
like Stitch (`ext let label = .. true >= 3 |> not - ( @ , false )`) rather than
`x`/`0` monotony. **Names are lowercase-only by correctness, not style**: the
oracle probes `Ident` with a lowercase representative and the parser branches
on identifier case (`starts_uppercase` separates a constructor pattern from a
binding), so emitting `Point` where `x` was approved would step outside what
was checked. Capitalised names need the deferred payload-aware `TokenSet`
refinement. A test pins that no wordlist entry lexes as a keyword — such an
entry would silently substitute a different token for the approved one.

**Increment 10 (protocol v0) done — as `kvetch-proto`, not `babble/src/proto.rs`,
and not postcard.** House convention (`fs-proto`, `glitch-proto`) encodes IPC
request/reply into `[u64; MSG_WORDS]` and versions by **append-only tags**, not
a version field; the plan's postcard guess was mine and conflicted with it.
Separate crate because the increment-13 client needs the message types but must
not link the sampler (and through it the whole Stitch parser) — the same reason
`fs-proto` is not part of `user/fs`. Design points:

- **One buffer, in and out.** `Complete { max_tokens, ptr, cap, prefix_len }`:
  the client's buffer holds the prefix on entry and receives the completion,
  read-style. Two separate buffers would not fit the four-word message.
- **No seed field, deliberately.** Entropy derives from the server's per-boot
  root plus a request counter, so a client cannot silently diverge snemu from
  hardware; itests pin the boot seed instead
  ([../docs/randomness-and-entropy.md](../docs/randomness-and-entropy.md)).
- `max_tokens` because a completion is a *fragment* — see the completeness
  correction above.
- Malformed input is a `WireError` to reply to, never a panic: unknown tag,
  prefix-exceeds-buffer, buffer-too-large, unknown status. 6 tests.

**Increment 11 (seed derivation) done** — `kvetch_proto::request_seed(boot_seed,
counter)`, SplitMix64's finalizer, pinned by golden vectors because a host-side
reproducer must derive the same seed the server used, on any engine, forever.
Takes no clock *by signature*, which is the enforcement of the seed-provenance
rule rather than a comment asking for it. **The golden test immediately earned
its keep**: `request_seed(0, 0)` was `0` — SplitMix64 maps zero to zero, and
boot-seed 0 (no `seed=` bootarg) with request 0 is the plainest run the system
has, so the most common path handed the sampler its most degenerate state.
Fixed with `counter + 1`; a property test now pins the non-degeneracy alongside
an avalanche test (a weak mix would correlate consecutive requests' completions,
since the counter is dense).

**Comments: decided, documented** — Stitch's lexer skips them, so they never
reach the grammar and the oracle needs no change; the question is purely about
the model. Settled as **input-only, loss-masked, uniform across every rung**:
a per-rung policy was drafted and rejected because differing training data
would not break speculative decoding's *correctness* but would collapse its
*acceptance rate* exactly where docstrings carry signal. The decisive argument
for never generating is **verification**, not capacity — comments are the one
artifact the parse/typecheck/test stack cannot check, so a confidently wrong
one passes every gate we have. Full reasoning in
[../docs/generative-ladder.md](../docs/generative-ladder.md). babble is the one
exception (it has no vocabulary) and may emit them behind a flag, off for
corpus generation.

**Increment 12 — pure core done and green; target glue written but not yet
boot-verified.** Split at the usual boundary:

- **`babble::serve::handle_request(buf, prefix_len, max_tokens, seed)`** — pure,
  host-tested, works on a byte slice rather than the wire's `(ptr, cap)`, so the
  one unsafe pointer conversion stays in the bin. Validates that the prefix is
  UTF-8 rather than trusting a cross-process claim. 6 tests.
- **Truncation never splits a token.** When the client's buffer cannot hold the
  whole completion it is cut at a *token boundary*, never mid-token — a
  half-written token can be a **different** token from the one the oracle
  approved (` without` cut to ` with` becomes a keyword). The first version of
  this test only checked "the buffer got shorter and is still viable", which
  naive byte truncation passes; it was replaced by "the served token stream is a
  genuine prefix of the untruncated one" at *every* buffer size, and verified by
  temporarily reverting the implementation — it caught `Ident("t")` where
  `Ident("task")` was approved.
- **`user/kvetch`** — server (`RECV`) + fixed-request client (`SEND`), mirroring
  `user/fs`: `receive_with_reply` → decode → `copy_from_caller` → sample →
  `copy_to_caller` → `reply`. Per-request span, `requests_total`,
  `bytes_emitted_total`, and **the seed on the wire** so a recorded completion is
  replayable. `BOOT_SEED = 0` until the `seed=` bootarg (increment 14) lands.
- Registered in `kernel/build.rs`, `trap/user.rs` (statics, specs, `LAYOUTS`),
  `main.rs` dispatch, and `WorkloadKind::KvetchBabble` (kebab-derived, one
  host test).

**Increment 13 (`kvetch-babble-serves`) done — and increments 12 + 13 are
verified end to end.** The on-target run and the host sampler agree exactly:

| | target (snemu guest) | host |
|---|---|---|
| seed | `-2152535657050944081` | `-2152535657050944081` |
| bytes | 34 | 34 |
| checksum | 6643581145736680012 | 6643581145736680012 |

…for the completion `greet(name) { "price" ( ) ~> not ..= true delta`. The
emitted seed equals `request_seed(0, 0)`'s golden vector, so the whole chain —
derivation, sampler, tables — is pinned across engines. Span nesting worked
too: `kvetch-client` → `kvetch.client` → `kvetch.complete` **on the server's
task id**, i.e. the trace crosses the process boundary unaided.

The scenario recomputes the expected completion host-side from the same crates
the server used and asserts seed, length and checksum. **Verified to actually
assert**: bumping the host's `max_tokens` by one made it fail with "the client
never reported 37 bytes". Passes plain and `--scramble`; full suite 126/126
both ways.

**Verification status:** host gate green apart from the mutant-plan roster,
which is missing three crates that belong to concurrent work
(`cram-corpus`, `kvetch-model`, `kvetch-vocab`) — `babble` and `kvetch-proto`
are already enrolled. `user/kvetch` is registered `NOT_HOST_TESTED` (riscv-only;
its logic is host-tested in `babble::serve`).

**Post material** (noted 2026-07-25): the raw babbled output — legal-but-
meaningless Stitch, every identifier `x` and every number `0` because the
oracle answers in *classes* and the sampler appends representative lexemes —
is a striking devlog visual. The narrative beat is that increment 7's terminal
synthesis makes it *plausible* after increment 6 makes it *whole*: first
legal, then finished, then plausible. **Found and fixed a real parser bug on the way**: the
three post-`bump` `self.err()` sites in `parse_atom`/`parse_pattern_atom`
blamed the token *after* the offending one (the caret pointed one token too
far right, and it made the oracle unsound); `expect` had always been
correct. One snapshot updated — it had pinned the buggy span.

TDD decomposition of
[../docs/babble-design.md](../docs/babble-design.md): the continuation
oracle in `stitch`, the seeded biased grammar-walk sampler, the batch CLI
(Tier-0 corpus hat), and kvetch v0 (the serving hat — babble behind an IPC
endpoint). Deliverables are the four contracts every later ladder rung
inherits: oracle API, bias-table format, completion-endpoint protocol v0,
eval floor. Entropy discipline per
[../docs/randomness-and-entropy.md](../docs/randomness-and-entropy.md):
seeds explicit, time never a parent, itests pin.

**Non-goals (explicitly later):** BPE mask-lifting (quip-time, needs a
vocab), the versioned-buffer protocol (stateless-with-prefix only), stim
consumers (affordances/highlighting), reseed epochs, type-directed TokenSet
narrowing, the full validator/metrics harness (the CLI emits programs + a
summary; corpus-pipeline integration is its own plan), parser-state
snapshot caching (stim-time optimization).

## Placement decisions (from the design doc's open questions)

- **Oracle lives in `stitch`** (`stitch/src/oracle.rs`) — it is parser
  knowledge; must stay no_std+alloc (serving hat runs it on-target).
- **Sampler lives in a new `babble/` crate** (no_std+alloc, host-buildable)
  consuming `stitch` — bias tables and wordlists are not parser business.
  The batch CLI is `babble`'s `[[bin]]` behind a `cli` feature (host-only);
  the library stays no_std for the kvetch bin.
- **Default bias tables are data** (`babble/tables/default.toml`, embedded
  via `include_str!`) — the shape-statistics pipeline later overwrites the
  file, not the code.
- **kvetch v0 is `user/kvetch`**, mirroring `user/fs` (serve loop, ELF
  embedded via the `SPAWNABLE` registry).
- **Protocol types live in `babble/src/proto.rs`** (postcard-encoded, pure,
  host-tested; shared by kvetch and future host clients).

Each increment is one RED→GREEN cycle: failing test first, in its own edit,
then minimum code, then mutants + refactor assessment. Host tests via
`cargo nextest run -p stitch -p babble`.

---

## Increment 1 — `TokenSet` + oracle trivial cases

**RED** (`stitch/src/oracle.rs` tests): `valid_next("", 0)` returns exactly
the token kinds that may begin a program (table pinned against the
grammar); `valid_next` at end of a complete tiny program includes EOF/none;
a position mid-keyword returns the continuation of that token (v0: the
"append at token boundary" contract — mid-token positions may return a
`ContinueToken` marker rather than a set; pin whichever contract we choose
in this test so the API can't drift silently).

**GREEN**: `TokenSet` (bitset over token kinds + `AnyIdentifier` /
`AnyLiteral` variants), `valid_next` for the empty/trivial paths only.

## Increment 2 — replay-with-sentinel across the grammar

**RED**: table-driven expected-sets through every major production —
`"after `use M.{`→ exactly {ident,`}`}"` style, one row per grammar
decision point (module header, fn params, match arms, handler clauses,
expression operators, call args). Rows are cheap; aim for one per
`expect`/`peek` site in the parser.

**GREEN**: the sentinel mechanism — parse the prefix with a sentinel
appended; every decision point records accepted kinds on sentinel-hit,
unwinds, tries alternatives. This is the bulk of the oracle work.

**MUTATE**: this is the increment where mutants earn their keep — a missed
alternative branch is exactly the bug class (an over-narrow mask silently
truncates babble's, later quip's, output space).

## Increment 3 — the maximal-munch test

**RED**: the call-paren gotcha, pinned: positions where char meaning
depends on lexer lookahead return the correct set (a call followed by `(`
calls its result — the oracle must agree with the lexer, not with
intuition). One focused test module; the standing wrinkle gets its
dedicated regression guard.

**GREEN**: thread lexer state into the char-facing view (kinds + lexeme
grammars per kind).

## Increment 4 — diagnostics become a view of the oracle

**RED** (characterisation first): existing parser error messages for three
representative malformed programs, snapshotted; then the new assertion —
`expected one of {…}` lists exactly `valid_next` at the failure position.

**GREEN**: parser error paths call the oracle (or share its tables) instead
of hand-maintained expected-lists. First consumer live; error messages
improve as a side effect. _(Skippable if it balloons — the oracle API is
the deliverable, not the refactor — but assess before skipping: it's the
increment that keeps oracle and parser honest against each other forever.)_

## Increment 5 — sampler: seeded determinism + membership

**RED** (`babble/src/lib.rs` tests): same seed + same tables → identical
byte output (two runs compared); every emitted token is a member of
`valid_next` at its position (the sampler validated against its own oracle,
re-checked post-hoc); different seeds → different output (smoke, not
proof).

**GREEN**: `Babble::new(oracle, tables, seed)` as an
`Iterator<Item = u8>`; PRNG is a small seeded xorshift/PCG (statistical
category — no CSPRNG, per the entropy doc).

## Increment 6 — bias tables: depth damping + termination

**RED**: with default tables, N seeds all terminate within a byte bound;
mean program length lands in a target band (loose — this is a tunability
assertion, not a golden value); a pathological table (no damping) is
_rejected at load_ rather than looping forever (table validation, not
runtime hope); tables round-trip from the TOML file.

**GREEN**: per-kind weights + depth-indexed damping; `Tables::load` with
validation; `include_str!` defaults.

## Increment 7 — terminal synthesis

**RED**: sampled `AnyIdentifier` renders a lexeme from the wordlist,
seeded-deterministic; rendered identifiers re-lex as single identifier
tokens (no accidental keywords — the wordlist is filtered against the
keyword set at load); literals render within their lexeme grammar.

**GREEN**: terminal generators behind one trait, wordlist embedded as data
beside the tables.

## Increment 8 — end-to-end: babble programs parse

**RED**: for seeds 0..N (N ≈ 100), collect the full emission and run the
real `stitch` parser over it — every program parses. (This is the design's
headline property, asserted wholesale; failures here after 1–7 are green
mean the oracle and parser disagree somewhere — the most valuable failure
this plan can produce.)

**GREEN**: nothing new if 1–7 were honest; fix what the property finds.

## Increment 9 — batch CLI

**RED** (CLI integration test, host): `babble gen --count 3 --seed 7`
writes three `.st` files + a `summary.json` (seed, per-program byte length,
token-kind histogram — the production-coverage stub); rerun is
byte-identical; `--seed` omitted is an error (no ambient entropy, even
ergonomically).

**GREEN**: thin `main` over the library; `cli` feature gate keeps the
library no_std.

## Increment 10 — protocol v0 encode/decode

**RED** (`babble/src/proto.rs` tests): request
`{version, seed, max_bytes, prefix}` and reply
`{version, status, completion}` round-trip through postcard; unknown
version decodes to a refusal-shaped status, not a panic; oversized prefix
is rejected at decode.

**GREEN**: the two structs + bounds. (Postcard field order is now wire
law — same reordering rule as `protocol::Frame`.)

## Increment 11 — seed derivation

**RED**: `request_seed(boot_seed, counter)` is pure, documented-stable
(pinned test vectors — this function is cross-engine wire law: host-side
replay must reproduce target-side draws forever), distinct across
counters, and contains no time input _by construction_ (signature takes no
clock — the test is the signature).

**GREEN**: one hash function (FNV/SipHash-class, seeded; not security
category).

## Increment 12 — kvetch v0, the userspace server

**RED** first as host tests for the pure core: `handle_request(req, oracle,
tables) -> reply` — valid request yields a completion that extends the
prefix legally; `max_bytes` respected; malformed request yields refusal
status. Then the target glue (thin, untested by unit tests per the
boundary rule): `user/kvetch` bin — serve loop on
`delegated_handle(0)`, per-request span with the **seed as an attribute**,
`kvetch.requests_total` / `kvetch.tokens_emitted_total` via
`RegisterMetric` (registered once at startup).

**GREEN**: the bin + `SPAWNABLE` registration + a `kvetch-babble` workload
arm (init-style: spawn kvetch, spawn a fixed-request client) in the
itest-workloads registry.

## Increment 13 — the itest: `kvetch-babble-serves`

**RED** (`xtask/src/itest/scenarios.rs`): boot `workload=kvetch-babble`;
assert (a) the client's reply arrives (a `Log`/metric frame the client
emits on success), (b) prefix+completion parses (harness-side, using host
`stitch`), (c) the completion is **byte-identical to the host sampler's
output** for the same seed/tables — the strong cross-engine determinism
assertion, and the reason increments 5–11 pinned everything, (d)
heartbeat survives. Runs under snemu; deterministic; joins the standard
gate including `--scramble`.

**GREEN**: whatever the boot path shakes out — this is the increment that
finds the integration surprises (staging, W^X, quota), by design.

## Increment 14 (optional, deferrable) — `seed=` bootarg

**RED** (`kernel-boot` bootargs tests): `seed=0xDEADBEEF` parses into the
boot config; absent → default-0 recorded as such. Kernel emits a
`boot_seed` frame during boot telemetry.

**GREEN**: one parse arm + one frame. Live-mode plumbing; nothing in 1–13
depends on it (the itest client pins its own seed).

---

## Eval-floor artifact (falls out, not an increment)

Increment 9's `summary.json` + increment 8's parse-rate are recorded once
as `babble`'s reference numbers (FIM test-pass 0%, idiom-match 0%, length
distribution = tables) — the chance-level row every trained rung is
measured against, per the ladder doc.

## Gate

`cargo xtask test && cargo xtask itest && cargo xtask itest --scramble`,
plus `cargo xtask clippy` and mutants over `stitch::oracle` and `babble`
(expect the usual disjoint-bitfield equivalent-mutant survivors in
`TokenSet`; document them when they appear).
