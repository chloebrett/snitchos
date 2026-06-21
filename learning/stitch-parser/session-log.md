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

## S2 — The AST as the dispatch target (~20 min)

**Review (S1 spaced):** 3/3. Maximal-munch *reason* (whitespace discarded → parser can't reconstruct `->`) recalled deeper than the textbook name. Re-entrant interpolation (lexer captures raw text → front-end re-parses it) re-tested and solid. `StrSegment` count predicted correctly (3: Lit/Interp/Lit). Interpolation deferral gap from S1 is **closed**.

**Covered:** the four AST families (`Item`/`Stmt`/`Expr`/`Pattern`) and which is file-scope-exclusive (`Item`). The statement-vs-expression fork — got that Stitch makes conditionals *expressions* and gave the payoff one-liner (`x = (y > 5 => "big" | "small")`). Then the session's core: **a method call has no dedicated node** — `obj.m()` parses as `Call{ callee: Field{...} }`, identical `Field` shape to a plain field read; meaning decided at eval time. Traced both trees correctly (minor naming: `callee` not `object`, `Var` not bare string). `Item::On`/`Item::Contract` + `Method`/`MethodModifier` shapes. `Method.body: Option<Expr>` → `None` = abstract contract signature (+ default-method subtlety noted). `Free` modifier + `@`/`SelfRef` = bug.

**Performance:** strong throughout — predicted the four families cold, nailed the `Call{Field}` decomposition (the plan's "20% that matters most" #3), and the closing Feynman synthesis traced parser→registry→evaluator completely and correctly *unprompted*. Honest about checking the code for the `None`-body question (good metacognition: recognized lookup vs recall).

**Key gap surfaced (the live one):** `register_items` in `interp.rs` silently drops `Item::On`/`Item::Contract` via its trailing `_ => {}` — methods aren't even *collected* yet. Step one of the eventual implementation. And today's `eval_field` errors "X has no field `m`" because there's no method-lookup fallback.

**Planted for S7:** the dispatch interception fork — special-case `Call` when callee is a `Field` (before evaluating the callee) vs. invent a "bound method" `Value`. Left unanswered deliberately.

**Bloom's reached:** Understand→Analyze on AST families; Apply on the `Call{Field}` decomposition; Evaluate (light) on the stmt/expr fork. Bridge-to-dispatch mental model is in place ahead of schedule.

**Confidence calibration:** 5/10 → 8/10. Well-calibrated — the 8 matches Apply/Analyze performance on the dispatch shapes, not inflated. (Fuzziest-part self-report: not given.)

**Next:** S3 — Pratt / precedence climbing (binding powers, the climbing loop, associativity). Trace `1 + 2 * 3` and `a + b |> f`. (S2 left the dispatch *shapes* clear; S3–S4 are the parser mechanics, S5 the `on`/`contract` declarations, S7 the implementation.)

## S3 — Pratt / precedence climbing (~25 min)

**Review (S1+S2 spaced, interleaved):** 3/3. `Call` vs `Field` outermost-node distinction sharpened to the dispatch contrast (`total(readings)` = `Call{callee: Var}` vs `readings.total()` = `Call{callee: Field}`). `register_items` `_ => {}` gap recalled. Maximal munch (lexer owns `..=`) recalled.

**Covered:** the tower-of-functions → single-loop+number upgrade. Binding-power *pairs* `(l_bp, r_bp)`; higher = tighter. The climbing loop + the `l_bp < min_bp` break. Hand-traced `2 + 3 * 4`, `2 * 3 + 4`, `8 - 2 - 1`, `a + b |> f` correctly. **Associativity from the pair asymmetry:** `(l, l+1)` → left-assoc; flip to `(l, l-1)` → right-assoc (lambdas get right-assoc structurally via `parse_expr(0)` body instead). Non-associativity (`a < b < c` rejected): the `(l,r)` numbers can't express it; `is_non_assoc(op)` + same-level-neighbor peek does. Closed on the recursive-vs-explicit-stack equivalence (shunting-yard mapping table).

**Performance:** strong. Traced every example correctly; derived left-assoc-from-`(11,12)` independently ("forced by it being (11,12) not (12,11)"). #1 non-assoc *why* answer was excellent — connected `a<b : Bool` to the JS `false==0` footgun AND noted the parser guards it *because* the dynamic tree-walk has no type checker yet (same "preview the static discipline" theme as S2). Asked unprompted whether Pratt could use an explicit stack — genuine transfer/Analyze-level curiosity; got the shunting-yard connection.

**Feynman:** captured the essence (call stack = implicit operand stack; one inequality threads precedence + associativity; no tower). Refined two vague bits: it's the recursion call stack (not a separate structure), and the decisive compare is *next op's `l_bp` vs current frame's `min_bp` (= enclosing op's `r_bp`)*.

**Confidence calibration:** 7/10. Self-aware: "recursion is a trip, I'd need a debugger to hit 9." Honest and accurate — head-knowledge solid, gut-knowledge pending a stepped trace.

**Self-study assigned:** `dbg!(min_bp, self.peek())` at top of `parse_expr`, run the `a + b |> f` test, read printed frames against the hand traces. Closes the recursion gap to ~9.

**Bloom's reached:** Apply→Analyze on the climbing loop (independent traces); Evaluate on the non-assoc design rationale and the recursive-vs-explicit-stack tradeoff.

**Next:** S4 — Lookahead & the tricky cases. lambda-vs-tuple (`parens_then_arrow`), placeholder→lambda desugaring (`$a`/`$`), the guard `=>` binding-power collision (already glimpsed: the `min_bp == 0` gate + branches parsed at `min_bp=1`). Lookahead vs backtracking made precise.

## S4 — Lookahead & the tricky cases (~35 min, turned into a code change)

**Review (S1–S3 spaced, interleaved):** 3/3. `a or b and c` associativity traced via the `l_bp < min_bp` break; `Call{Field}` 3-deep nesting; no `Newline` token (greedy/maximal-munch separates statements). Caught and corrected my sloppy framing on statement separation: maximal munch is strictly a *lexer* term (longest token); the parser reuses the same *greedy* principle at statement level. The call-then-`(` prelude gotcha is that greed biting.

**Covered — the three genuinely-ambiguous constructs, all resolved without backtracking:**
1. **lambda-vs-tuple** (`parens_then_arrow`): scan to the *matching* `)` (depth counter — `(x, (y, z)) -> …` is why a naive first-`)` scan breaks), check for a following `->`. Pure **lookahead**: iterates tokens, never bumps `self.pos`, never rewinds. Learner gave the depth-counter counterexample and the lookahead-vs-backtracking distinction cleanly (closes the stated S1 gap).
2. **placeholder → lambda** (`collect_placeholders` + `parse_arg`): `$` desugars to a lambda *per call argument*; the traversal stops at `Expr::Lambda` so an inner explicit lambda keeps its own `$` (`map(xs, x -> filter(x, $.ok))` → `$.ok` is filter's). Learner self-corrected my botched `f($) + g($)` example (it's `f($a->$a) + g($a->$a)`, identity into each — they were right, I was wrong; acknowledged).
3. **guard `=>` collision**: guard parsed at `parse_expr(1)` not `(0)` so the arm's `=>` separator isn't swallowed by the conditional handler (which only fires at `min_bp==0`). Learner nailed it unprompted: "start with 1 to never bind it."

**Design detour → shipped a change.** Learner challenged the placeholder arity rule. Surfaced three candidate semantics (occurrence-order / position-by-letter / sort-by-name); learner argued for **position-by-letter** (their original intent). Decisive argument (mine, after they pushed): #2's gaps become *ignored params*, letting `$b` alone *select* the second positional arg (`(_, $b)`) — sort-by-name can't express that. **Found the impl had drifted from the spec:** `docs/language-design.md:119` already says "Arity = highest letter referenced" (#2), but `parser.rs` used a `BTreeSet` → sort-by-name (#3), silently. So this was a spec-conformance fix, not a new feature.

**Implemented (TDD, learner directed "you do it"):**
- Decision note added to `docs/language-design.md` (the three rules + why #2 + the mnemonic-names cost).
- RED: parser snapshot test `placeholder_gap_becomes_an_ignored_param` (`f($a + $c)`) — was `["$a","$c"]`, want `["$a","_","$c"]`.
- GREEN: new `positional_params(&BTreeSet) -> Option<Vec<String>>` — letter = index, arity = max letter, unreferenced lower slots → `"_"`. `parse_arg` calls it.
- Added runtime behavior test `a_placeholder_gap_ignores_the_skipped_argument` (`apply($b)` over `g(10,20)` → 20) — covers the distinct eval path (the `_` hole binds-and-ignores).
- 288 + 11 + 3 green; clippy clean. Non-gap (contiguous) cases unchanged — purely additive.

**Performance:** excellent. Two independent correct challenges to *my* explanations (the `f($)+g($)` desugaring; pushing on arity semantics until it became a real design decision). This is Evaluate/Create level — driving language design, not just reading the parser.

**Open language question parked:** whether #2's "a letter IS its index" (so `$x` = arg 24, mnemonic names meaningless, needs an arity cap) is the final rule, vs. keeping a small alphabet. Noted in the design doc.

**Bloom's reached:** Evaluate→Create (drove a spec-conformance change + design rationale). Lookahead-vs-backtracking gap from S1 closed.

**Confidence calibration:** not formally rated (session ran into implementation). Performance suggests high on lookahead + placeholder mechanics.

**Next:** S5 — Declarations (`prod`/`sum`/`func`/`contract`/`on` → AST). The direct dispatch prerequisites: how `on Type { … }` and `on Type : Contract { … }` and `contract` parse into `Item::On`/`Item::Contract`. Then S6 patterns, S7 the dispatch implementation.

## S5 — Declarations: the dispatch prerequisites (~20 min)

**Review (S2–S4 spaced):** 2.5/3. `f($b)` → `(_, $b) -> $b`, 2 params (S4 solid). Guard-`=>` trace at `parse_expr(0)` → "expected `|`" error (S3/S4 solid). **Miss:** confused `Item::On.contract: Option<Type>` (conformance, `: C` clause) with `Method.body: Option<Expr>` (abstract method). Corrected — two distinct `Option`s, both central to dispatch. Worth re-testing at S6.

**Covered:**
- `parse_item` dispatch: 5 keyword arms + **function has no keyword** (`Token::Ident(_)` → `parse_func`). Learner got the payoff: top-level is declarations-only (no bare expressions — the S2 `Item`-is-file-scope-exclusive point), so `parse_func` can **commit** on an ident with no lookahead/backtrack.
- `contract` (`require_body=false`) vs `on` (`require_body=true`) share `parse_method`. Edge cases: **method-with-body in a contract = default method** (named it); **method-without-body in an `on` = parse error** (nothing to dispatch to).
- The data model: the sample (`prod Celsius` + `contract Show` + `on Celsius : Show`) → three `Item`s (`Prod`/`Contract`/`On`).

**Keystone — derived the full runtime dispatch algorithm (the S7 target):**
1. value → `type_name`; 2. find `On` items with `target == type_name`, scan `methods` by name → run `body`; 3. not found but block has `contract: Some(C)` → follow *that pointer* to `contract C`'s default `body`; 4. else "no method".

**Standout insight (unprompted):** learner questioned whether the interpreter needs contracts at all — "only the compiler cares about conformance, not the interpreter?" Correct and architecturally deep: conformance checking (`: Show` ⇒ does Celsius implement all of Show? orphan/coherence) is **static**; the dynamic tree-walker consults a contract **only** for default-method fallback. That deletes a layer from the S7 impl.

**Corrections applied:** (a) the two `Option`s mix-up (review). (b) Feynman step-3 "look at all contracts for defaults" — *I* misread "all" as global; learner clarified they meant "all contracts whose `on Abc` blocks were scanned," which is **correct and more precise than my "one pointer"**: a type can have *multiple* `on` blocks (`on Abc : Show`, `on Abc : Eq`), so the default-fallback set is the union of contracts named across all of Abc's `on` blocks. Only contracts that *no* `on Abc` block named are out of scope (coherence). (c) merged "find `on Abc`" and "find `on Abc : C`" — one lookup over `On` items by target; the `: C` clauses matter only for the fallback set.

**Bloom's reached:** Evaluate→Create (derived the dispatch algorithm + independently separated static-conformance from dynamic-dispatch). Ahead of plan — this was scheduled for S7 synthesis.

**Confidence calibration:** 8/10. Matches the Evaluate/Create performance — solid.

**Next:** S6 — Patterns (`parse_pattern`): wildcard/literal/binding/constructor/tuple/or-patterns; the uppercase=constructor vs lowercase=binding convention; how destructuring parses. Re-test the two-`Option`s distinction. Then S7: learner implements dispatch (the algorithm is now fully specified above).

## S6 — Patterns (~15 min) — completes the parser tour

**Review:** skipped at learner's request (S1–S5). Two-`Option`s re-test folded into the close instead.

**Covered:**
- **The capitalization rule** (`parse_pattern_atom`): `Ident` + `starts_uppercase` → `Pattern::Constructor`; lowercase `Ident` → `Pattern::Binding`. Learner nailed the *why* immediately: **parsing happens before any symbol table exists**, so the parser can't ask "is `Circle` a known variant?" Capitalization is a purely syntactic, context-free, backtrack-free signal. Cost: convention is load-bearing (no lowercase variants / uppercase bindings). Same trick as Haskell/Elm/Erlang.
- The `Pattern` node zoo: `Int/Float/Bool/Str` (literal), `Wildcard` (`_`), `Binding`, `Constructor{name, args}`, `Tuple`, `Or`.
- **Nesting = recursion:** `Constructor.args: Vec<Pattern>` parsed via `parse_pattern` per element. Traced `Ok(Some(x))` → Constructor→Constructor→`Binding("x")` correctly (caught the leaf is a `Binding`, not the token `Ident`, after a nudge).
- **Or-patterns one level up:** `parse_pattern` parses an atom, then collects `|`-separated alts into `Pattern::Or` — the outer wrapper, not bare-nested.
- **Tuple-vs-grouping** (mirrors S4 lambda-vs-tuple): `(x)` → after `pats.pop()`, empty remainder ⇒ return the single pattern unwrapped (grouping); `(x, y)` ⇒ `Tuple`. Signal = "was there a comma" (list length). Learner read it straight off the code.
- `@degrees` → `Field{object: SelfRef, name}` (expression side; method-body receiver access).

**Review miss closed:** the two `Option`s, re-tested clean — `Item::On.contract: Option<Type>` is `None` for `on Celsius {}` / `Some(Show)` for `on Celsius : Show {}`; `Method.body: Option<Expr>` is `None` for an abstract contract signature. S5's confusion resolved.

**Sharpened:** `_` (Wildcard) vs `Binding("unused")` — `_` introduces **no binding** (can't be referenced; the canonical "deliberately ignored" marker, = the hole the S4 placeholder fix emits); a `Binding` does bind, just goes unused.

**Bloom's reached:** Apply→Analyze. Feynman terse but captured both load-bearing ideas (capitalization rule + recursion).

**Confidence calibration:** S6 7/10. Learner felt ready for S7 and **started wiring up the `On` block dispatch themselves** before the session formally opened — exactly the build-it-yourself goal of the whole track.

**Next:** S7 — **learner implements dispatch.** Parser tour complete (S1–S6). The runtime algorithm is fully specified in the S5 log + cheat sheet: (1) `register_items` must stop dropping `Item::On`/`Item::Contract` (the `_ => {}`) and build a method registry keyed by `type_name`; (2) `eval_call`/`eval_field` gain a method-lookup fallback (`Call{callee: Field}` → find method by name on the value's type → contract-default fallback). TDD, learner-driven.

## S7 — Learner implements dispatch (basic `on X`) — THE PAYOFF ✓

The whole track's destination. Learner drove the implementation; I reviewed/guided each step and wrote the red test staircase + two helpers on request. **Scope: basic `on X` (inherent methods) only; `on X : C` contract default-method fallback explicitly deferred.**

**What shipped (all green: 292 + 11 + 3, clippy clean modulo one no-arg `format!` to drop):**
- **Registry** — `register_items` now handles `Item::On`, accumulating methods into `HashMap<String, Vec<Method>>` keyed by `type_name` via the `entry().or_default().extend(...)` idiom (after a first pass fought `get_mut`+`insert` and `Copy`-on-`Vec`). Multiple `on` blocks per type accumulate (S5 insight, test-locked).
- **Env bridge** — added a `methods: Rc<OnceCell<HashMap<String, Vec<Method>>>>` field (mirroring `globals`, same letrec rationale), propagated in `bind`, `set_methods` setter, `lookup_method(type, name) -> Option<Method>` reader (two-key, find-by-name, clone-out), and a `globals_only()` hygiene helper (shared Rcs, `locals: None`) so method bodies run in global scope, not the caller's locals — the closure-hygiene principle applied to methods.
- **Dispatch** — new guarded `eval` arm `Expr::Call { callee, args } if callee is Field`, placed *before* the generic call arm (the S2 interception fork: split the Field, eval object→receiver, get `Data.type_name`, `lookup_method`, arity-check, bind params + receiver-as-`@` on a `globals_only` env, eval `method.body`).
- **`@`** — new `Expr::SelfRef` arm resolving the receiver bound under reserved name `"@"`; `@field` flows through the existing `Field` arm for free.

**Teaching arc that worked:** the red-test staircase (each test isolating one concern) drove incremental wiring — `dispatches_an_inherent_method` (`@`-free) went green on dispatch alone, then `SelfRef` + receiver-binding lit up the other three in one step. Diagnosed exactly as predicted.

**Gotchas surfaced (learner-corrected with guidance):** `matches!` binds nothing (re-destructure in body); rust-analyzer auto-imported `std::intrinsics::unreachable` (the *function*) instead of the prelude macro → `core_intrinsics` E0658; `?` on `Option` in a `Result` fn (→ `ok_or_else`); `let Some(Value::Data(..)) = value` (value isn't an `Option`); `unreachable!` on a non-`Data` receiver is a real error path, not unreachable; `.or_else(|_| Err(new))` discards the informative original error.

**Concepts taught alongside:** what a `Value` is (Token→Expr→Value progression; runtime tag = the type; Rc payloads; Method-is-a-recipe-not-a-Value); why `OnceCell` (capture-before-fill / letrec); what `letrec` is and its etymology (`let` + `rec`; introduce-then-fill; vs `let`/`let*`); the `entry` API vs get_mut+insert.

**Deferred (next session candidates):** `on X : C` contract conformance + default-method fallback (the `contract: Option<Type>` field is captured but unused; `Item::Contract` still dropped); method-not-found error wording test; the no-arg `format!` clippy cleanup; `mut`/`free` method modifiers at eval time.

**Bloom's reached:** Create — learner built a working language feature from a self-derived spec, debugging real compiler errors. Track destination reached.

**Confidence calibration:** (not formally rated — implementation flow; demonstrated competence by driving the impl with only spot guidance).
