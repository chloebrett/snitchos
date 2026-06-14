# Session log — Stitch parser

## S1 — Lexer & tokens (~25 min)

**Covered:** source → `Token`s; token classes (Ident vs Int literal — Int carries a parsed `i64`, not text); `->` as one token (maximal munch in the lexer); whitespace fully discarded (no `Newline` token); string-interpolation lexing.

**Performance:**

- Token prediction: strong. One miscategorization — wrote `Ident("2")`, self-corrected to `Int(2)` when prompted with "what can start each class?". Solid retrieval.
- Maximal munch (evaluate-level): pushed back hard ("too fragile, why not newlines?") — _good_ judgment. Came around after pricing newline-significance: realized multi-line list literals work for free under whitespace-insignificance and that newlines would _force_ an ASI-style continuation ruleset. Reached the conclusion mostly solo.
- Interpolation: initially modelled it as a flat token stream with brace-delimiter markers (choice (a) — a real design, but not this one). Gap surfaced: didn't see how deferred re-parsing works ("don't we want to tokenize expressions?"). Taught the **re-entrant front-end** mechanism (`parse(&raw)` re-runs lexer+parser on captured raw text). Then traced the nested `"outer {f(\"inner {y}\")}"` case and **named the recursion correctly**. Landed it.

**Gaps / for review:** the word-fishing on "lexer stays \_\_\_" fell flat — avoid vague prompts; user wants mechanism. Re-test interpolation deferral at S2 review.

**Confidence calibration:** TBD (asked at close).

**Bloom's reached:** Understand→Evaluate on lexer design; Apply on the recursion trace.

**Next:** S2 — the AST as the target (`ast.rs`), flag dispatch-relevant shapes (`Call{Field}`, `SelfRef`).
