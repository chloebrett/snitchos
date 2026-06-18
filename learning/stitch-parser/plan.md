# Learning plan — the Stitch parser (→ contract dispatch)

**Learner profile:** has written a JSON parser + a small tree-walk scripting language; solid on automata/regex/recursive descent. Gaps: specifics of *this* parser, and lookahead/"backtracking". Wants the full parser tour, examples-first, building toward implementing `on`/method + `contract` dynamic dispatch in the evaluator.

**Destination:** understand `stitch/src/{lexer,ast,parser}.rs` well enough to confidently add method-call dispatch to `interp.rs`.

**Style:** examples-first (trace real inputs through real code, extract the principle), full tour, multi-session.

## The 20% that matters most
1. **Pratt parsing** — binding powers + the climbing loop. The conceptual upgrade over per-precedence-level recursive descent.
2. **Lookahead vs backtracking** — `parens_then_arrow` (lambda-vs-tuple), placeholder desugaring, the `=>` guard binding-power trick.
3. **The AST shapes the evaluator walks** — esp. `Call{callee: Field{..}}` for `obj.m()` and `@`/`SelfRef`. This is the bridge to dispatch.

## Session outline (spiral; revisit shapes at increasing depth)
- [x] **S1 — Lexer & tokens.** Source → `Token`s as a state machine. Novel bit: string-interpolation lexing (a string is a template, not an atom). ✓ landed the re-entrant `parse(&raw)` mechanism + recursion.
- [x] **S2 — The AST as the target.** `ast.rs`: Expr vs Item vs Stmt vs Pattern. Frame everything as "the parser builds these." Flag the dispatch-relevant shapes early. ✓ Nailed `Call{Field}` decomposition + parser→registry→evaluator synthesis. Surfaced the live `register_items` `_ => {}` gap; planted the S7 interception fork.
- [x] **S3 — Pratt / precedence climbing.** Binding powers, the loop, associativity. Trace `1 + 2 * 3` and `a + b |> f`. ✓ All traces correct; derived associativity-from-pair independently; got non-assoc rationale + recursive-vs-shunting-yard equivalence. Conf 7/10 (debugger self-study assigned for the recursion gut-check).
- [ ] **S4 — Lookahead & the tricky cases.** lambda-vs-tuple, placeholder→lambda desugaring, the guard `=>` collision. Lookahead vs backtracking, made precise.
- [ ] **S5 — Declarations.** `prod`/`sum`/`func`/`contract`/`on` → AST. The dispatch prerequisites.
- [ ] **S6 — Patterns.** match patterns; uppercase=constructor convention; how destructuring parses.
- [ ] **S7 — Bridge to dispatch.** Synthesize: how `Call{Field}`, `@`/SelfRef, and `on`/`contract` AST feed the dispatch you'll write. Then you implement.

## Progress
- Session log: `session-log.md`. Cheat sheet: `cheat-sheet.md` (built as we go).
- Status: **S1–S3 done. S4 (lookahead & the tricky cases) next.**
