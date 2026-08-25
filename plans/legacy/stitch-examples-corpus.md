# Stitch examples corpus — 30 programs

**Status: ✅ COMPLETE — all 30 programs shipped.** `examples/stitch/` carries 30
programs, ~6,300 lines and **279 native tests**, every one parsing, type-checking and
passing its own suite under `stitch/tests/examples.rs`. Write-up:
[stitch 18](../../posts/stitch-18-a-binding-is-not-a-boundary.md); findings:
[stitch-examples-findings.md](../stitch-examples-findings.md).

The exercise paid for itself the way it was designed to — the friction *was* the data.
It found a real interpreter defect (`Env::bind` reset `self_closure`, silently disabling
the tail-call trampoline for any function that bound a name before its recursive call —
i.e. most of them), corrected `docs/language-design.md`'s `mut` aliasing semantics in
three places, added two natives (`Str.parseInt`, `toStr`), and turned up four
manifestations of one maximal-munch grammar fact plus two stdlib types (`Set<T>`,
`Either`) that were designed and never built.

**What the exemplars turned out to be worth**, measured afterwards in
[batch11-training-findings.md](../../notes/batch11-training-findings.md): swapping the 24
that reach training for an equal token count of generated corpus costs ~0.022 nats —
about **20× per token** against generated Stitch. That figure is a *ceiling*, not a point
estimate, pending the deconfounding pair described in that note's "Open" section.

Tracking doc, as originally written, for a batch of new `.st` example
programs, proposed and approved 2026-07-26. Each is ~100+ lines (1000 is a
guideline, not a cap — let the program's natural size win), lands in
`examples/stitch/`, and includes native `test "…" { expect … }` blocks (see
[stitch-testing-design.md](../../docs/stitch-testing-design.md)) giving **adequate
coverage of its core logic — every program's tests must pass before it's
ticked off.** The batch doubles as hand-polished gold-exemplar material for
[corpus-mvp.md](../corpus-mvp.md)'s recipe-matched prompting, not just a REPL demo
corpus like `fs-image/`.

**Findings note:** [stitch-examples-findings.md](../stitch-examples-findings.md)
— one running doc for all 30, updated as each program lands. Record anything
non-obvious: language limitations hit, workarounds, awkward-to-express
constructs, gaps in the stdlib/prelude, checker surprises. This is the point
of the exercise as much as the programs themselves — it's read-the-language-
by-writing-a-lot-of-it, and the friction is the data.

Read [../docs/language-design.md](../../docs/language-design.md) before writing
any of these — surface syntax, `prod`/`sum`/`contract`/`on`, `uses`
capabilities, `use <-`, placeholders — and skim the existing corpus
(`fs-image/{primes,double}.st`, `fs-image/lib/{text,stats,greet}.st`,
`fs-image/stim/stim.st`, `stitch/src/prelude.st`, `plans/lang/samples.st`) for
house style: doc comments explaining *why*, not what; `uses` rows kept
minimal; telemetry spans on anything worth watching in Grafana.

Work through the list in order unless told otherwise. Tick a box when the
program is written, type-checks (`cargo xtask test` runs the canon-style
checks the whole workspace shares), and its `test` blocks pass.

## Batch 1 — first proposal

- [x] 1. `json.st` — hand-rolled JSON parser/printer (`sum Json`), recursive
      descent, `Result`/`?`.
- [x] 2. `bank.st` — ledger with `mut` accounts, `uses Telemetry` + `uses
      FsWrite` audit log, capability-gated `on` methods.
- [x] 3. `vm.st` — toy stack-based bytecode interpreter (`sum Instr`),
      tail-recursive step function.
- [x] 4. `graph.st` — BFS/DFS/topo-sort/connected-components over
      `Map<Str, List<Str>>`, no mutation. (Landed as `List<AdjEntry>`, not
      `Map` — see findings doc, entry 1.)
- [x] 5. `markov.st` — Markov-chain text generator, seeded LCG, `uses
      Telemetry` span.
- [x] 6. `sched.st` — priority vs round-robin task scheduler simulation via
      `contract Schedulable`.
- [x] 7. `calc.st` — infix expression parser/evaluator with variables,
      `Result`-typed errors (div-by-zero, unbound var).
- [x] 8. `inventory.st` — pluggable pricing via `contract PricingStrategy`,
      per-category telemetry gauges.
- [x] 9. `life.st` — Conway's Game of Life over `List<List<Bool>>`, pure
      `step`, `text.st`-based renderer. (Renderer written inline, not via
      `use text` — examples parse standalone under the batch's own gate;
      see corpus doc's Notes section.)
- [x] 10. `shell.st` — command dispatcher/mini-REPL, capability-gated
       commands, `use <-` + `?.`.

## Batch 2 — second proposal

- [x] 11. `regex.st` — tiny regex engine (literal/`.`/`*`/`|`/anchors),
       recursive backtracking matcher.
- [x] 12. `csv.st` — CSV parser/writer, quoted-field handling, `Result`-typed
       row parsing.
- [x] 13. `trie.st` — persistent prefix trie, immutable insert/lookup/
       prefixSearch.
- [x] 14. `lru.st` — immutable-value LRU cache (`get`/`put` return a new
       cache), memoized Fibonacci on top.
- [x] 15. `elo.st` — Elo rating calculator, `contract RatingSystem` with two
       `on` strategies (fixed-K vs margin-scaled). (Elo's real formula is
       infeasible — no `pow`/`exp`/`sqrt`/`Float` math at all; landed as a
       documented integer piecewise-linear approximation — see findings.)
- [x] 16. `tictactoe.st` — board + win detection + minimax AI. (Written
       after 18–22, correcting the ordering slip below. Found a new variant
       of the maximal-munch gotcha — a `let … = … |> f(args)` statement
       followed by a line starting with `(` fuses the same way; see
       findings.)
- [x] 17. `maze.st` — BFS pathfinding over a wall grid, tuple coordinates.
- [x] 18. `huffman.st` — Huffman coding tree, encode/decode round-trip test.
       (Found & fixed a real bug in this batch's `Str.split(s, "")`
       "characters of a string" idiom — see findings.)
- [x] 19. `diff.st` — LCS-based line diff (`sum DiffLine`), DP via nested
       `fold`.
- [x] 20. `tokenbucket.st` — rate limiter, `mut` bucket state, per-request
       telemetry span.
- [x] 21. `circuitbreaker.st` — `sum State = Closed|Open|HalfOpen` state
       machine, `on State : Show`.
- [x] 22. `spellcheck.st` — Levenshtein distance over a dictionary. (`Set<Str>`
       doesn't exist in the interpreter at all — landed as `List<Str>` +
       `contains`; see findings.)

**Note (2026-07-26):** items 18–22 were written before 16–17 — an
ordering slip while working through "do the next 5" autonomously (picked
up the list past `elo.st` one item too far in). Not reordering the list
itself; 16–17 were written afterward to close the gap, so all of 11–22 are
now done regardless of the order they landed in.
- [x] 23. `formula.st` — spreadsheet cell graph, cross-references, cycle
       detection (`Result`), memoized eval.
- [x] 24. `template.st` — mustache-like `{{var}}`/`{{#each}}` renderer over
       nested context. (`List<Binding>`, not `Map` — and hit the `{{`/`}}`
       escaping trap for real, plus a genuine argument-order bug of my
       own; see findings.)
- [x] 25. `semver.st` — semantic-version parse/compare/sort, first real
       `Either` usage. (`Either` isn't a builtin either — self-declared.
       Found the maximal-munch gotcha's third variant, at match-arm
       boundaries; see findings.)
- [x] 26. `interval.st` — merge-overlapping-ranges / max-non-overlapping
       interval scheduling.
- [x] 27. `queue.st` — producer/consumer queue simulation, throughput via
       a locally-reimplemented `summarise` (cross-module `use` doesn't work
       under this batch's standalone-per-file gate; see findings).
- [x] 28. `poker.st` — 5-card poker hand ranking (`sum HandRank`), grouping/
       sorting via association lists, designed from the start to avoid the
       match-arm maximal-munch gotcha semver.st found.
- [x] 29. `handlers.st` — `handle`/`with` demo: a mock `Telemetry` handler
       recording emits into a list, used to unit-test `primes.st`-style
       code purely in Stitch.
- [x] 30. `logstats.st` — log-line parser (`LEVEL: message`), per-level
       counts + top-N words, observability-themed capstone.

**All 30 programs complete.** Every file parses, type-checks, and passes
its own tests under `stitch/tests/examples.rs`; the full workspace gate
stayed green throughout. See plans/stitch-examples-findings.md for the
full write-up, including two real interpreter/checker findings that led to
fixes (`Str.parseInt`/`toStr` added to `natives.rs`; the `Env::bind`
self-tail-call regression fixed) and one documentation fix
(`docs/language-design.md`'s `mut`-aliasing claim corrected).

## Notes

- File location: `examples/stitch/<name>.st` (new top-level dir — this repo
  has no `examples/` yet; distinct from `fs-image/`, which is the *bootable
  filesystem seed*, and from `plans/lang/samples.st`, which is an
  illustrative feel-doc, not validated code).
- Each program should stand alone (load independently), but is free to reuse
  `fs-image/lib/{text,stats}.st`-style helpers via `use` where it's a natural
  fit (see #27's planned reuse of `stats.st`).
- No numbering prefix needed on the files themselves — the ordering above is
  a work queue, not a naming scheme.
- **Definition of done per program:** file written, type-checks, its `test`
  blocks pass, adequate coverage of the interesting paths (not just the happy
  path), and [stitch-examples-findings.md](../stitch-examples-findings.md)
  updated with anything worth recording before the checkbox is ticked.
- **Gate:** `stitch/tests/examples.rs` (new) parses + type-checks every file
  under `examples/stitch/` and asserts every `test` item in every file
  passes, with a control test proving the gate can fail. Run the whole thing
  with `cargo nextest run -p stitch --test examples`, or just the parse/type
  pass with `... examples::every_example_parses_and_type_checks_clean`. This
  is the "every single one needs to pass its tests" requirement, made
  mechanical rather than a manual promise.
