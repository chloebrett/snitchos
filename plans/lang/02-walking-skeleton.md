# Stitch — walking-skeleton plan (v0)

_Turn the grammar spec ([01](01-grammar-and-precedence.md)) into a running tree-walk interpreter, TDD throughout. Single host crate `stitch` (std), lib-first so every layer is unit-testable._

## Crate

- **`stitch/`** — host crate, edition 2024, `[lints] workspace = true`, lib-first.
- Layout grows by increment; no empty speculative modules (YAGNI):
  - `src/lexer.rs` — source → `Token`s.
  - `src/ast.rs` + `src/parser.rs` — Pratt parser off the §2 precedence table.
  - `src/value.rs` + `src/interp.rs` — tree-walk evaluator.
  - `src/main.rs` — a `.st` runner / REPL (last, once the lib evaluates).
- No external deps to start. `insta` (dev) arrives at the parser stage for AST snapshots; until then, plain `assert_eq!`.

## TDD increment order

Each is a red→green→(refactor) loop; each leaves the crate green.

1. **Lexer** — tokens for the §1 lexical grammar. ← *start here.*
2. **AST + Pratt parser** — expression precedence (§2), then declarations. `insta` snapshots on the AST.
3. **Eval: literals + arithmetic/boolean/comparison** — the expression core.
4. **`let` + lexical scope; functions + closures.**
5. **`prod`/`sum` construction + field access; `match`** (incl. subjectless, guards, or-patterns).
6. **Pipes + placeholders; eager `List`/`Map` + literals + finite ranges + eager combinators** (`map`/`filter`/`fold`/`each`/`find`/`toList`).
7. **`?` / `?.` + built-in `Maybe`/`Result`.**
8. **Lazy `Seq` + infinite producers** (`iterate`/`repeat`/`forever`) + `takeWhile`/`foldWhile` + lazy ranges.
9. **`span`/`emit` host stubs + `use <-`.**

Throughout: **dynamically typed** (type annotations parsed, not checked); `uses` parsed-then-ignored; `on`/`contract` methods after the data core (slot near 5–6).

Deferred to later plans: static types/inference, capabilities effect-checking, real telemetry (`Frame`), the bytecode VM, the GC, modules/visibility.
