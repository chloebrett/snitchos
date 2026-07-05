# Mutation testing for Stitch — a tool that's cheaper because we own the language

**Status: design / exploration (captured 2026-07-05). Pre-implementation.**
A sibling milestone to [stim](stim-design.md): a minimal mutation-testing tool for
Stitch programs. Motivated directly by the stim architecture decision — the editor
FSM lives in Stitch (`.st`), and the project's existing quality gate
(`cargo-mutants`, see the `mutation_testing_setup` memory) mutates **Rust only**.
This tool extends the gate to Stitch, closing the one methodological hole in
"the editor is a Stitch program."

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

A `xtask stitch-mutants <program.st>` (or a `stitch` sub-tool): three operators
(arithmetic swap, comparison swap, bool-literal flip), AST-walk mutant generation,
a fuel-capped interpreter run against a supplied test closure, a killed/survived
report. Then widen operators and add the `test_*` convention. Run it over stim's
editor FSM as the first real target.

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
- `stitch/src/ast.rs`, `stitch/src/parser.rs`, `stitch/src/interp.rs` — the AST +
  parser + interpreter the tool reuses.
