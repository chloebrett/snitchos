# Stitch examples corpus — 30 programs

**Status: 📐 PLAN — not started.** Tracking doc for a batch of new `.st` example
programs, proposed and approved 2026-07-26. Each is ~100+ lines (1000 is a
guideline, not a cap — let the program's natural size win), lands in
`examples/stitch/`, and includes native `test "…" { expect … }` blocks (see
[stitch-testing-design.md](../docs/stitch-testing-design.md)) giving **adequate
coverage of its core logic — every program's tests must pass before it's
ticked off.** The batch doubles as hand-polished gold-exemplar material for
[corpus-mvp.md](corpus-mvp.md)'s recipe-matched prompting, not just a REPL demo
corpus like `fs-image/`.

**Findings note:** [stitch-examples-findings.md](stitch-examples-findings.md)
— one running doc for all 30, updated as each program lands. Record anything
non-obvious: language limitations hit, workarounds, awkward-to-express
constructs, gaps in the stdlib/prelude, checker surprises. This is the point
of the exercise as much as the programs themselves — it's read-the-language-
by-writing-a-lot-of-it, and the friction is the data.

Read [../docs/language-design.md](../docs/language-design.md) before writing
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
- [ ] 16. `tictactoe.st` — board + win detection + minimax AI. (Skipped out
       of order — see note below; still owed.)
- [ ] 17. `maze.st` — BFS pathfinding over a wall grid, tuple coordinates.
       (Skipped out of order — see note below; still owed.)
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
itself (the work is done and correct under its real numbers); 16–17 are
still owed and get done next.
- [ ] 23. `formula.st` — spreadsheet cell graph, cross-references, cycle
       detection (`Result`), memoized eval.
- [ ] 24. `template.st` — mustache-like `{{var}}`/`{{#each}}` renderer over
       nested `Map`/`List` context.
- [ ] 25. `semver.st` — semantic-version parse/compare/sort, first real
       `Either` usage.
- [ ] 26. `interval.st` — merge-overlapping-ranges / max-non-overlapping
       interval scheduling.
- [ ] 27. `queue.st` — producer/consumer queue simulation, throughput via
       `stats.st`'s `summarise` (cross-module `use`).
- [ ] 28. `poker.st` — 5-card poker hand ranking (`sum HandRank`), grouping/
       sorting via `Map`.
- [ ] 29. `handlers.st` — `handle`/`with`/`without` demo: a mock `Telemetry`
       handler recording emits into a list, used to unit-test `primes.st`-
       style code with no real capabilities.
- [ ] 30. `logstats.st` — log-line parser (`LEVEL: message`), per-level
       counts + top-N words, observability-themed capstone.

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
  path), and [stitch-examples-findings.md](stitch-examples-findings.md)
  updated with anything worth recording before the checkbox is ticked.
- **Gate:** `stitch/tests/examples.rs` (new) parses + type-checks every file
  under `examples/stitch/` and asserts every `test` item in every file
  passes, with a control test proving the gate can fail. Run the whole thing
  with `cargo nextest run -p stitch --test examples`, or just the parse/type
  pass with `... examples::every_example_parses_and_type_checks_clean`. This
  is the "every single one needs to pass its tests" requirement, made
  mechanical rather than a manual promise.
