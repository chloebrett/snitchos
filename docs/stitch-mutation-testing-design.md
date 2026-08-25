# Mutation testing for Stitch — a tool that's cheaper because we own the language

**Status: design / exploration (captured 2026-07-05). Pre-implementation.**
A sibling milestone to [stim](stim-design.md): a minimal mutation-testing tool for
Stitch programs. Motivated directly by the stim architecture decision — the editor
FSM lives in Stitch (`.st`), and the project's existing quality gate
(`cargo-mutants`, see the `mutation_testing_setup` memory) mutates **Rust only**.
This tool extends the gate to Stitch, closing the one methodological hole in
"the editor is a Stitch program."

> **Update (2026-08-09): the milestone is smaller than written, and the first
> target has changed.** Both of the design's open questions — "what are the
> tests" and the non-termination cap — were closed by work that shipped after
> this page was captured (see *Prerequisites, now met*). Separately, the
> 30-program [examples corpus](../plans/legacy/stitch-examples-corpus.md) now exists
> and is a better first target than stim's FSM, **and** its
> [findings](../plans/stitch-examples-findings.md) are direct evidence about
> which mutation operators pay. Both are folded in below.

Do **not** gate stim on this. stim v1's FSM is covered by ordinary behavior tests
through the interpreter harness; this tool *audits* those tests once it exists.

---

## Thesis

Mutation testing measures test effectiveness: apply a small semantic change (a
*mutant*) to the code, run the tests, and if no test fails the mutant *survived* —
a gap in the suite. `cargo-mutants` does this for Rust. Stitch programs are
currently untestable this way. Building a Stitch mutation tester is not only
feasible but **cheaper and faster than the Rust tool it models**, for two reasons
that fall out of owning the whole language.

## Why it's cheap — two structural advantages

1. **We own the AST, so mutant generation is trivial.** `cargo-mutants` spends its
   complexity budget parsing Rust and rewriting source safely. Stitch already has a
   clean `Expr`/`ast.rs` enum and a parser — mutate the tree directly. A first set
   of operators, all a few lines each over the AST:
   - arithmetic: `+`↔`-`, `*`↔`/`
   - comparison: `<`↔`<=`↔`>`↔`>=`, `==`↔`!=`
   - boolean: `&&`↔`||`, flip a bool literal
   - integer literal: `n`↔`n±1`, `n`↔`0`
   - structural: delete a `match` arm, replace a block body with a no-op/unit
   - **argument swap: exchange two adjacent same-typed call arguments** — added
     2026-08-09 on corpus evidence, see below

   **Which operators pay is now an evidence question, not a guess.** Writing the
   30-program corpus surfaced real bugs in real Stitch, and they cluster on
   operators this list originally missed:
   - `template.st` — `renderEach(List.removeAt(items, 0), body)` where
     `renderEach(body, List.removeAt(items, 0))` was meant: **two arguments
     transposed**, surfacing two calls away as `"no match arm matched"`. An
     arithmetic-swap operator would never generate this; an argument-swap
     operator generates it directly. Hence its addition above.
   - `poker.st` — `isConsecutive` deliberately avoids the cheaper-looking
     `max - min == 4`, which is wrong on a hand with a duplicate rank. A test
     suite that would accept the cheap version has a real gap, and the
     boundary/off-by-one operators are what expose it.
   - `vm.st` — a stack-machine exit path that discarded the accumulator instead
     of the spent counter, invisible on the two smallest inputs.

   The lesson for operator selection: this corpus's bugs are **data-shape and
   argument-position** errors far more than arithmetic ones. Weight the operator
   set accordingly rather than copying a Rust tool's defaults.

2. **No compile-per-mutant — the surprising win.** `cargo-mutants` is slow because
   it *recompiles* Rust for every mutant. A tree-walk interpreter just re-runs the
   mutated AST: milliseconds, no build step. Stitch mutation testing runs at
   mutants-per-second where the Rust tool runs at mutants-per-minute. (Devlog
   angle: "mutation testing my toy language — faster than the industrial tool,
   because there's no compiler in the loop.") *Compounding lever:* the tool re-runs
   the interpreter once per mutant, so interpreter throughput **is** mutants/sec — a
   **release build** of the interpreter (a potential ~20x win, basically free — see
   the snemu precedent) directly multiplies this. Today everything runs in debug.

## Mechanism sketch

```
for each mutable AST node in program.under_test:
    for each applicable operator:
        mutant = program with that node rewritten
        result = run_tests(mutant)          # drive the interpreter
        classify: killed (a test failed) | survived | timed-out
report survivors, killed %, per-operator breakdown
```

- **Run under a fuel cap.** A mutant can loop forever (mutating a recursion base
  case; Stitch has recursion + `Seq`). The interpreter run takes a **max-eval-steps
  budget**; exceeding it is a distinct `timed-out` bucket (treated as killed or
  reported separately — a policy knob). The interpreter wants a step budget anyway.
- **"The tests"** — v0: a caller-supplied test closure/command that drives the
  interpreter and reports pass/fail. v1: a tiny Stitch test convention (`test_*`
  functions + an `expect`/`assert` native) so tests live in `.st` alongside the
  code, and the tool discovers them.

### Prerequisites, now met (2026-08-09)

Both bullets above were written as work still to do. Both shipped since:

- **The test convention exists**, in a better form than the `test_*` naming
  convention proposed here: `test "name" { expect … }` is a first-class item
  (`Item::Test`), discovered and driven by `test_runner::run_tests`. The corpus
  already carries 280+ assertions across 30 files, and
  `stitch/tests/examples.rs` loads and runs every one.
- **The fuel cap exists**, and so does the bucket: `test_runner::DEFAULT_FUEL`
  is 1,000,000 steps, and `Verdict::Exhausted` is already a *distinct* verdict
  from `Verdict::Failed` — exactly the three-way classification
  (killed / survived / timed-out) the mechanism sketch asks for.

So the tool no longer needs to invent a test convention or a termination
policy. It needs the AST walk, the operators, and a report — it reuses
`run_tests` as-is for everything else.

**One caveat the corpus exposes**, worth knowing before the first run: `expect`
asserts a `Bool` and there is no way to assert that an expression *faults*
(recorded at the end of the findings doc). So a mutant that turns a passing
computation into a **runtime fault** is killed for the right reason, but a
mutant that should have been caught by a refusal-style test cannot be — those
tests do not exist in Stitch. Mutation scores on capability-heavy programs will
read optimistically until `expect fault` (or similar) lands.
- **Scope.** Mutate one program (or module) at a time; the mutable-node walk is a
  pure pass over the parsed AST, reusing the existing parser + interpreter with no
  new evaluation machinery.

## Caveats (all bounded, none worse than Rust)

- **Equivalent mutants** — a mutant semantically identical to the original is
  unkillable by construction (the classic false-positive). Same problem as the Rust
  setup, which already documents one; manage the same way (annotate + exclude).
- **Non-termination** — handled by the fuel cap above.
- **Operator coverage vs. noise** — start with the high-signal operators listed;
  resist adding operators that mostly produce equivalents.

## First milestone

A `xtask stitch-mutants <program.st>` (or a `stitch` sub-tool): four operators
(arithmetic swap, comparison swap, bool-literal flip, **argument swap**), AST-walk
mutant generation, a fuel-capped run, a killed/survived report. The test-closure
and `test_*`-convention steps are **no longer needed** — call `run_tests` with
`DEFAULT_FUEL` and read the `Verdict` (see *Prerequisites, now met*).

**First target: the examples corpus, not stim's FSM** (revised 2026-08-09). The
corpus did not exist when this page was written. It is now the better target on
every axis: 30 programs against one, 280+ assertions already written,
`stitch/tests/examples.rs` already loading and running them, and a
[findings log](../plans/stitch-examples-findings.md) recording what each
program's tests were *meant* to catch — so a survivor can be read against the
author's stated intent instead of guessed at. It also spans registers (parsers,
interpreters, DP tables, state machines, `mut`-heavy state) where stim's FSM is
one shape.

Run stim's FSM second — it remains the motivating case, and by then the tool
will have been calibrated on a corpus whose gaps are already documented.

## Prior art

`cargo-mutants`, `mutmut`/`cosmic-ray` (Python), Stryker (JS), PIT (Java) — all
mutate a language they don't own, paying the parse/rewrite (and, for compiled
languages, the recompile) tax. **A mutation tester built *by the language's own
author*, sharing the parser + interpreter, with no compile step**, is an unusually
cheap instance of an old idea — and a clean dogfooding story for a language whose
host project treats mutation testing as a first-class gate.

## Cross-references

- [stim-design.md](stim-design.md) — the editor whose Stitch FSM this audits.
- `mutation_testing_setup` memory / the `mutation-testing` skill — the Rust gate
  (`cargo-mutants`) this extends to Stitch.
- [../plans/legacy/stitch-examples-corpus.md](../plans/legacy/stitch-examples-corpus.md) and
  [../plans/stitch-examples-findings.md](../plans/stitch-examples-findings.md) —
  the 30-program first target and the evidence behind the operator set.
- `stitch/src/ast.rs`, `stitch/src/parser.rs`, `stitch/src/interp.rs` — the AST +
  parser + interpreter the tool reuses.
- `stitch/src/test_runner.rs` — `run_tests`, `DEFAULT_FUEL`, `Verdict` (the
  prerequisites that landed after this page was captured).

## Note on scope: this does not cover the interpreter

Worth stating because it came up while building the `Map` natives: this tool
mutates **Stitch programs**, not the Rust that runs them. `cargo-mutants` owns
the latter — but has its own blind spot there. A collection native's body
(destructure, loop, construct, return) contains no operator, comparison, or
boolean for cargo-mutants to perturb, so it generates only a whole-function
replacement, which is *unviable* where the return type has no `Default`. Two
consecutive `Map` increments produced six mutants, all unviable — a green report
carrying no signal. The mutants that mattered (reversed entry order, transposed
key/value, `List` returned where a `Map` was owed) had to be applied by hand.

Neither tool covers that gap; it is a reason to keep hand-mutation in the loop
for data-shape code, not a feature request for either.
