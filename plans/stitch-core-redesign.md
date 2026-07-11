# Plan: Stitch core redesign (tokenizer · parser · interpreter)

**Status**: Active
**Lands on**: `main`, incrementally (no feature branches; the user commits each
known-good increment). Phases below are the review/commit milestones.

## Goal

Execute the [Stitch tokenizer/parser/interpreter redesign review](../docs/redesign-reviews/stitch-tokenizer-parser-interpreter.md)
as an **incremental, in-place evolution that keeps all 504 tests green at every
step** — not a rewrite. The review was written cost-blind (that's the exercise);
this plan is the cost- and risk-aware path to the same destination.

Scope: items 1–5 + 7 (spans, reified evaluator, faithful-AST/IR, interning,
effects, cleanups). **Item 6 (GC) is explicitly out** — it rides the bytecode-VM
milestone (user decision). A **static type checker** is a **parallel track**
(VM-independent; see below). The bytecode VM itself is a later milestone this
redesign is deliberately the prework for.

## Why now, and why this order

The review's own logic: retrofitting spans + a shared IR is "far cheaper before
stim + the mutation tester + a VM all bind to this AST than after." **Phases A–D+F
are common prework for *both* futures** (stim-on-tree-walker *and* bytecode-VM):
the VM compiles the Phase-C core IR, Phase B is the seed of the VM's execution
loop, and the effect semantics (D) must be defined on the IR before either backend.
So the stim-vs-VM decision doesn't bite until after D — and this plan defers it to
there, informed by the Phase-B leak measurement (below).

## Keep-green discipline (the whole risk story)

- Every step leaves 504 (+ new) tests green. The existing suite is the
  characterization net for a large refactor.
- **Do not churn the AST twice.** The exact-tree AST tests (`PartialEq` snapshots)
  are the biggest churn cost. Token spans + parse/lex diagnostics + interning
  (Phase A) touch the AST only at *leaf* nodes (a `Symbol` swap). **Node-level
  spans land in Phase C**, folded into the surface/core AST reshape that is already
  churning the AST — so the exact-tree tests are rewritten once, not twice.
- Pure-refactor steps (e.g. B1 reify) rely on the suite as the safety net: no
  behaviour change, tests unchanged.

---

## Phase A — Spans (tokens + diagnostics) — ✅ DONE (A1–A3)

Delivers real parse/lex error messages (line:col + caret) and stops the lexer
silently swallowing garbage. A1–A3 shipped 2026-07-05 (521 tests green, mutation-
clean). **Interning (former A4) moved to Phase C** — by this plan's own
"don't churn the AST twice" rule, `Symbol`-in-the-AST belongs with C's AST reshape
(the resolution pass), and an isolated lexer-only version delivers ~zero (intern
then immediately un-intern to a still-`String` AST) while forcing a global-interner
decision. So Phase A is complete at A1–A3.

### Step A1: `Span` + tokens carry position
**Acceptance**: `lex` yields `Token { kind: TokenKind, span: Span{start,end} }`
(byte offsets); the parser reads `.kind`; every existing lexer/parser test green.
**RED**: a lexer test asserting a token's span for a known input.
**GREEN**: split today's `Token` enum → `TokenKind`; wrap with a span; thread
`.kind` through `peek`/`bump`/matches.

### Step A2: Spanned parse errors + caret rendering
**Acceptance**: `ParseError` carries a `Span`; a renderer prints `line:col` + a
caret line; the "expected X, found {got:?}" debug-print (`parser.rs:600`) is gone.
**RED**: a test asserting a specific parse error's rendered line:col + caret.
**GREEN**: thread the offending token's span into `ParseError`; add the renderer.

### Step A3: Lexer error channel — no silent miscompiles
**Acceptance**: `lex` returns `(Vec<Token>, Vec<LexError>)`; a malformed/overflowing
numeric literal is a spanned `LexError`, not `parse().unwrap_or(0)` (`lexer.rs:174`);
an unrecognized char is a spanned error, not a silent skip (`lexer.rs:128`).
**RED**: a test that an overflowing integer literal and a stray `` ` `` each produce
a spanned error (today both are swallowed).
**GREEN**: split whitespace-skip from unknown-char (`lexer.rs:294,344`); emit errors.

*(Former Step A4 — interning — folded into Phase C; see the Phase A note above.)*

*PR boundaries (shipped): A1 "tokens carry byte-offset spans"; A2 "spanned parse
errors with caret rendering"; A3 "lexer error channel — report bad literals + stray
chars."*

---

## Phase B — Reified evaluator + fuel + trampoline (the one-thing pick)

The execution context the runtime has no home for today (free `eval_*` fns
threading `&Env`). Also produces the **leak measurement** that informs the post-D
stim-vs-VM decision.

**Re-scope (2026-07-05):** the full free-fns→methods reify of `interp.rs` (2958
lines) is too big for one known-good increment, and `Env` already carries
run-ambient shared state (telemetry, platform, authority via `Rc`). So the *fuel*
budget lands as a fifth run-ambient `Env` field (`Rc<Cell<u64>>` + `with_fuel` +
`take_fuel`, decremented once per `eval`) — **✅ DONE**: `eval_program_with_fuel`,
a non-terminating program now faults "evaluation fuel exhausted" instead of
hanging; 523 green, mutation-clean. The **depth guard** (B3) — **✅ DONE**: a
run-shared depth counter + a `CallGuard` (RAII decrement on every exit, incl. `?`)
at the `apply_values` closure boundary; unbounded *non-tail* recursion (which the
trampoline can't fix) now faults "call stack too deep" instead of overflowing.
⚠️ `MAX_CALL_DEPTH = 48` is **deliberately low + target-dependent**: `eval` is a
giant function (huge stack frame — itself a Phase-C target), so deep recursion
overflows at a low depth on constrained stacks (esp. the metal's 16 KiB). Raise it
once the eval frame shrinks. Only the **self-tail trampoline** (B4) — real
`eval_call` restructuring — remains, plus **B5** the leak characterization. The
full struct-reify is deferred as a code-org refactor.

### Step B1: Reify `Interp` — `eval_*` become methods (pure refactor)
**Acceptance**: an `Interp` struct owns the eval entry points (env access,
telemetry/platform handles, and — as fields for later steps — fuel + stacks);
`eval`/`apply_values`/etc. are methods; **behaviour unchanged, 504 green**.
**RED**: none new (pure refactor); the suite is the net. *(Exception to test-first,
noted: a behaviour-preserving mechanical reshape guarded by the full suite.)*
**GREEN**: introduce `Interp`, move the free fns onto it.

### Step B2: Fuel / step budget
**Acceptance**: `Interp` decrements `fuel` per eval step; exhaustion raises a
*catchable* `Fault` (or distinct `Interrupted`), never a hang; a configurable
budget (unbounded by default so existing tests are unaffected).
**RED**: a non-terminating program halts with the fuel fault under a small budget.
**GREEN**: a per-step decrement + check in `eval`.
*Why it's load-bearing:* the [Stitch mutation tester](../docs/stitch-mutation-testing-design.md)'s
fuel cap **is** this.

### Step B3: Call-frame stack + depth guard
**Acceptance**: `Interp` maintains an explicit call-frame stack; deep Stitch
recursion raises a catchable stack-overflow `Fault` instead of a Rust abort; the
frame stack is the future home of spanned fault backtraces (Phase C).
**RED**: unbounded non-tail recursion faults catchably at a depth limit.
**GREEN**: push/pop frames in `apply_values`; guard depth.

### Step B4: Self-tail-call trampoline
**Acceptance**: a self-tail-recursive Stitch function runs in bounded Rust stack
(e.g. a tail loop to 1e6 completes without overflow).
**RED**: a tail-recursive counter to a large N returns without overflow.
**GREEN**: trampoline self-tail-calls in `apply_values` (`interp.rs:806`).
*Why:* this is what makes a **Stitch-hosted stim loop** viable (vs. the native
trampoline the stim plan assumed).

### Step B5: Characterize the "leaks per-run" (investigation → written finding) — ✅ DONE (2026-07-08/09)

Confirmed: `Rc` cycle `EnvInner → globals BTreeMap → Closure { env: Env } → EnvInner`.
50× linear growth over 50 `eval_program` calls. Measurement in `stitch/tests/memory_churn.rs`
(tracking `#[global_allocator]`). **Fixed structurally in C4 (upvalue capture)**:
closures store `upvalues: Vec<(String, Rc<RefCell<Value>>, bool)>` + 
`home_globals: Weak<OnceCell<BTreeMap<String, Value>>>` instead of `env: Env`.
The `Weak` breaks the cycle — `Rc<OnceCell>` ref count hits 0 when the env drops.
`memory_churn` test now shows 0 B growth over 50 runs. 543 tests green.

---

## Phase C — Faithful surface AST + one lowering pass → core IR

The big structural piece and the true VM prework. **Spans originate on the surface
AST and flow into the core IR**; a single lowering pass replaces the in-place
desugars; `Value::Closure` holds a code-ref (`Rc<CoreExpr>`) not a cloned `Expr`; the
evaluator runs the core IR; runtime `Fault`s cite `line:col`.

**Sequencing decision (2026-07-10):** two goals were bundled here — *spanned faults*
(real value; needs the surface `Expr` to carry spans, since lowering sees `Expr`, not
tokens) and the *core-IR refactor* (structural, no behavior change). Because spans
must originate on the surface `Expr` either way, we do **surface spans first** (C2),
so every later step is born with real spans and stays testable. **Identifier
interning is CUT from Phase C** — it was folded in under "don't churn the AST twice,"
but that rule protects the *surface* AST (544 pinned tests), and interning happens
*during lowering* (surface→core), so it never touches surface `Expr`. CoreExpr is an
internal type with a handful of structural tests; a later `String → Symbol` swap
there is cheap. Interning's real payoff (O(1) slot lookup) is a VM-milestone concern
a `BTreeMap`-globals tree-walker doesn't need — **deferred to the bytecode-VM
milestone** (see below).

**Current state (2026-07-10, after B, F, C1 completion):**
- Parser already emits faithful surface AST — `SubjectlessMatch`, `Placeholder`,
  `OperatorRef` all survive; the "faithful surface AST" goal is already achieved.
- `lower.rs` desugars in place by mutating `Expr` values; produces no separate IR
  type. Current desugars: `SubjectlessMatch` → nested `If`; `OperatorRef` → Lambda;
  `Placeholder` → `Var + Lambda`; `Stmt::Use` → callback-lambda.
- `ClosureData.body: Expr` — owned clone of the body expr; no code-ref sharing.
  Upvalues landed 2026-07-09 (former "C4").
- `ParseError` carries a real span (C1 done). Surface `Expr`/`Stmt` nodes carry **no**
  spans; `Env` depth is a counter only — no frame stack, no per-call location.
- 544 tests green.

**Acceptance**: surface `Expr` carries per-node spans; desugaring lives in exactly one
surface→core pass; tree-walker evaluates the core IR; runtime faults cite `line:col`;
544+ green.

---

### Step C1: ParseError carries a real span — ✅ DONE (2026-07-10)

`ParseError { message: String, span: Span }` existed but span was always
`Span::default()`. Added `Parser::current_span()` + `Parser::err(msg)` helper;
replaced all 21 `ParseError::new(...)` call sites with `self.err(...)` (or
`parser.err()`/`sub.err()` in free functions). Interpolation sub-parser now
propagates its own span. 10 insta snapshots updated to show real byte offsets.
544 tests green.

---

### Step C2: Surface `Expr` carries per-node spans — ✅ DONE (2026-07-10)

Wrapped the AST: `Expr { kind: ExprKind, span: Span }`; the old `enum Expr` became
`enum ExprKind` (variants unchanged — children stay `Box<Expr>`, so spans nest).

**Two decisions that killed the churn:**
- `PartialEq for Expr` compares **only `.kind`** — structural equality assertions in
  tests ignore spans, so no `assert_eq!` test churned.
- `Debug for Expr` **forwards to `.kind`** — every `insta` tree snapshot printed
  identically to before, so **zero snapshot churn** (the plan budgeted for accepting
  ~40; we accepted none). Spans get dedicated span tests instead.

Spans originate in `parser.rs` via three helpers (`cur_start`, `prev_end`,
`spanned(start, kind)`): atoms carry their token span; binary/postfix/call nodes span
from the leftmost operand's start through the last consumed token. `lower.rs` and
`interp.rs` (both doomed in C4) got mechanical `.kind` matches + `Expr::bare(...)`
constructions with default spans — real spans there don't matter, they're deleted in
C4. `Stmt`/`Pattern` spans deferred (faults cite expression positions; add if C5
needs them).

New span tests (`an_atom_carries_the_span_of_its_token`,
`a_binary_span_covers_both_operands`,
`a_call_span_runs_from_callee_through_the_closing_paren`) + the C1 error-span test.
**556 lib tests green, zero snapshots churned.** Two pre-existing clippy warnings
(`index` unused, one `collapsible_if`) remain in the doomed `eval` — left for the C4
deletion.

---

### Step C3: Define CoreExpr + lowering produces it (with real spans) — ✅ DONE (2026-07-10)

New `src/core_ir.rs` (named `core_ir`, not `core`, to avoid shadowing the `core`
crate in this `no_std` lib) defines `CoreExpr { kind: CoreExprKind, span: Span }`
plus `CoreExprKind`, `CoreArg`, `CoreStrSegment`, `CoreStmt`, `CoreMatchArm`,
`CoreItem`, `CoreMethod`. `CoreExprKind` is `ExprKind` minus the surface-only nodes
(`SubjectlessMatch`, `OperatorRef`, `Placeholder`); `Spread` stays. `Pattern`,
`Field`, `Variant`, `Param`, `Type`, `MethodModifier` are reused from `ast`
unchanged. Names stay `String` (interning deferred). Like `Expr`, `CoreExpr`'s
`PartialEq`/`Debug` ignore/forward the span.

- `CoreExprKind::Lambda { body: Rc<CoreExpr> }` and `CoreItem::Func { body: Rc<CoreExpr> }`
  use `Rc` — closures share a code-ref instead of deep-cloning.
- `CoreStmt` has only `Let`/`Assign`/`Expr`.

**Implementation choice**: `lower_expr_to_core` = `clone` → existing in-place
`lower_expr` (the tested surface→surface desugar) → `to_core` (a pure, total 1:1
reshape; `unreachable!` on any surviving surface-only node). This reuses the tested
desugar logic rather than reimplementing the four desugars in core-building form.
C4 note: when the in-place path is deleted, `lower_expr`'s desugar helpers stay (they
back `lower_expr_to_core`); only the old `eval` and the `lower_program` entry point
retire. `lower_item(s)_to_core` lower `Func`/`Const`/method bodies; type metadata
passes through.

New tests: the four desugars (`SubjectlessMatch`→`If`, `OperatorRef`→2-param
`Lambda`, `use <-` block→`Call` result, `Placeholder`→`Lambda` arg), span
preservation, and item lowering (body desugared, type decl passes through). **564
lib tests green**, zero churn.

**Mutation deferred to C4**: `to_core` is mechanical 1:1 mirroring; it gets full
behavioral coverage for free once C4 wires `eval_core` and the whole 564-test suite
runs through the core path (a wrong arm mapping would fail there). The C3 tests cover
the actual desugar logic, which is what carries risk.

---

### Step C4: Interpreter evaluates CoreExpr; delete the old path — ✅ DONE (2026-07-10)

The runtime now evaluates the core IR end to end; `ClosureData.body: Rc<CoreExpr>`;
one evaluator. **564 lib + 26 feature-gated integration tests green through the core
path; clippy clean.**

**How it was done** (differs slightly from the pre-plan below): rather than write
`eval_core` *alongside* the old `eval`, the existing `eval` + expr-walking helpers were
**retyped in place** to walk `CoreExpr` (`&Expr`→`&CoreExpr`, `ExprKind::`→
`CoreExprKind::`, `&[Arg]`→`&[CoreArg]`, `&[Stmt]`→`&[CoreStmt]`,
`&[StrSegment]`→`&[CoreStrSegment]`, `pattern::eval_match` → `&[CoreMatchArm]`). Since
nothing evaluates surface `Expr` after the cut, keeping the names `eval`/`eval_tail`
(now walking core) was less churn than a `_core` split and left no duplicate to delete.
- `ClosureData.body: Expr → Rc<CoreExpr>` (value.rs). Lambda arm shares the `Rc`
  (no deep clone) and uses `free_vars_core` for upvalue capture. `eval_safe_field`'s
  accessor builds an `Rc<CoreExpr>` body. `registry::register_items` lowers each
  `Func` body via `lower_expr_to_core`.
- **Method bodies** stay `ast::Method`/`Expr` in the method table and are lowered to
  core **on-the-fly** at dispatch (`eval_method_call`, `call_instance_method`) — no
  `env.rs`/registry method-table refactor. `apply_values` + `natives.rs` unchanged
  (Value-level). External eval callers (REPL `runner::eval_line`, `testing::run`/
  `run_err`) lower the parsed expr with `lower_expr_to_core` before eval.
- `eval`'s match became total over `CoreExprKind`; the old `_ =>` "not implemented"
  fallback is replaced by an explicit `Spread` error (spread is arg-position only).

**Follow-up cleanup (behavior-neutral, deferred):** the 6 in-place `lower_program(&mut …)`
calls in the entry points + REPL are now **redundant** (func bodies lower via
`lower_expr_to_core`; methods lower on-the-fly), and surface `free_vars`/`lower` are
orphaned. Safe to delete in a follow-up (all downstream consumers —
`collect_exports`/`manifest_of_main`/`check_coherence`/`link_imports` — read item
names/types, not expr bodies). Left in place to keep this cutover's diff tight.

**Mutation (deferred from C3):** `to_core` + the desugars now get strong behavioral
coverage — every construct in the 564-suite is evaluated through `lower_expr_to_core`
+ `eval`(core). An explicit `cargo mutants` pass on `lower.rs`/`core_ir.rs` is a
nice-to-have follow-up.

**Groundwork (2026-07-10):** `lower::free_vars_core(&CoreExpr, &BTreeSet<String>)`
— the CoreExpr analog of `free_vars`. Additive.

**The cutover is atomic** — `ClosureData.body` can't be both `Expr` and `Rc<CoreExpr>`,
so there is no green intermediate until it all lands. Execute as one focused push in
this order, then run the 564-suite:

**Seam map (from the interp.rs survey):**
- `ClosureData.body: Expr` is **read at exactly one site**: `apply_values` →
  `eval_tail(&closure.body, …)` (interp.rs ~922). Constructed at 3 sites: the `Lambda`
  eval arm (interp.rs ~516), the `eval_safe_field` accessor (~1207), and
  `registry::register_items` for `Item::Func` (~92).
- `apply_values` and **all of `natives.rs` are Value-level** — natives need zero
  changes (`NativeFn.func` is `fn(&[Value], &Env)`; the 13 `apply_values` call sites
  all pass `&Value` + `&[Value]`).

**EXPR-WALKING functions to mirror as `_core` (take `&CoreExpr`/`&[CoreArg]`/
`&[CoreStmt]`/`&[CoreMatchArm]`/`&[CoreStrSegment]`):** `eval`→`eval_core`,
`eval_tail`→`eval_tail_core`, `eval_call`, `eval_method_call`, `eval_pipe`,
`eval_cross_pipe`, `stage_name`, `construct` (handles `CoreExprKind::Spread`),
`eval_string`, `eval_range`+`eval_int`, `eval_block`, `assign_place`,
`is_assignable_place`, and the `eval_safe_field` accessor (builds an `Rc<CoreExpr>`
body now). In `pattern.rs`: `eval_match(&Value, &[CoreMatchArm], …)` — its helper
`try_match` is Value-level and reused unchanged (`CoreMatchArm.pattern` is `ast::Pattern`).

**VALUE-LEVEL, reused as-is:** `eval_binary`/`eval_unary`/`as_bool` (ops.rs),
`apply_values`, `make_data`, `eval_index`, `eval_field`, `eval_try`, `range_seq`,
`assign_binding`, `rebuild_with_field`, `some`/`none`, `try_match`.

**Two `Expr` dependencies to resolve:**
1. **Method bodies** (`env.lookup_method` returns `ast::Method` with `body: Option<Expr>`).
   Decision: **lower method bodies to core on-the-fly** in `eval_core`'s method path
   (`lower_expr_to_core(&method.body)` then `eval_core`). Keeps the method table as
   `Vec<Method>` — no `env.rs`/registry method-table refactor. Noted perf follow-up
   (re-lowers per call; cache or store `CoreMethod` later).
2. **Lambda upvalues** use `free_vars` over the body — `eval_core`'s `Lambda` arm uses
   `free_vars_core` (done) over the `Rc<CoreExpr>` body.

**Edit order:**
1. `ClosureData.body: Expr → Rc<CoreExpr>` (value.rs).
2. Write `eval_core` + all `_core` mirrors (interp.rs) + `eval_match` core arm (pattern.rs).
3. Fix the 3 `ClosureData` construction sites to build `Rc<CoreExpr>` bodies
   (Lambda arm uses `free_vars_core`; accessor builds a core `Field`; registry lowers
   the func body via `lower_expr_to_core`).
4. `apply_values` → `eval_tail_core(&closure.body, …)`.
5. Wire entry points: `build_env_in` / the `eval_program_with_*` fns lower the
   top-level expr + items to core and call `eval_core`. Check const-value evaluation.
6. Delete the old `eval` + expr-walking helpers + the `lower_program` entry point
   (the desugar helpers `lower_expr`/`lower_block`/etc. stay — they back
   `lower_expr_to_core`).
7. Run the 564-suite; fix. Then mutation-check the desugars + `to_core` (deferred
   from C3) now that behavioral coverage flows through the core path.

**Post-cutover expectation:** all 564 green through `eval_core`; `body: Expr` gone;
one evaluator.

---

### Step C5: Spanned faults — ✅ DONE (2026-07-10)

`RuntimeError::Fault(String)` → `Fault { message: String, at: Option<Span> }`.
`eval` (and `eval_tail`) split into a thin wrapper + `*_dispatch`; the wrapper
stamps the current `expr.span` onto an unlocated fault as it propagates out
(`RuntimeError::stamped` no-ops once `at` is set, and passes `Return`/`TailCall`
control signals through untouched) — so a fault cites the **innermost**
sub-expression that produced it. `RuntimeError::span()` exposes it. Only 3 `Fault`
sites existed (all in value.rs, everything else goes through `new`), and no test
compared `RuntimeError` by equality, so churn was minimal — existing `run_err`
tests use `.message()` and were unaffected.

Tests: `a_runtime_fault_carries_the_span_of_the_offending_expression` (`1 / 0` →
`0..5`) and `a_fault_cites_the_innermost_offending_subexpression` (`4 + (1 / 0)` →
the inner `5..10`, not the outer `+`). **567 lib + 26 integration green, clippy
clean.**

**Deliberately deferred (each its own concern, not a one-liner):**
- **Rendering `line:col` + caret** (à la `ParseError::render`) needs a **SourceMap**:
  runtime spans come from *multiple independently-parsed sources* (prelude, user
  program, REPL defs), whose byte offsets overlap, so a fault span alone can't be
  rendered against "the" source. A `SourceMap` (source-id per span, or per-closure
  provenance) is the natural next step to make faults user-visibly cite a location.
- **Frame stack / backtraces** — replacing `depth: Rc<Cell<u32>>` with a
  `Vec<CallFrame>` was in the original C5 sketch, but it only enables backtraces,
  which nothing renders yet. Left as YAGNI; the depth guard still works as the
  recursion backstop.

---

### Phase C non-goals (deferred to VM milestone)

- **Identifier interning** (`String → Symbol` in CoreExpr + slot-indexed `Env`
  locals). Cut from Phase C (2026-07-10): its payoff is O(1) slot lookup, which a
  `BTreeMap`-globals tree-walker doesn't need, and it never touches the surface AST,
  so it can't cause a double-churn there. A later swap on the internal CoreExpr type
  is cheap. Lands with the bytecode VM, whose execution model actually wants slots.
- Full bytecode compilation of CoreExpr
- Generics / type parameters in CoreExpr
- Multi-shot continuations
- Inlining / dead-code elimination passes

---

## Phase D — Effects: structured `uses` row + runtime handler-stack

**Phase goal**: `uses: Vec<String>` → a structured, **spanned effect row**;
`authority: Rc<BTreeSet<String>>` → a runtime **handler stack** so effects can be
intercepted / redirected / attenuated over a block's dynamic extent (the membrane
stim's modes need). Single-shot only — no resumable continuations (VM territory).

**Current state (2026-07-11 survey):**
- `uses: Vec<String>` on `Item::Func` + `Method`; bare names, no span. `parse_uses`
  parses `uses Cap, Cap`. `ClosureData.uses: Option<Vec<String>>`.
- `authority: Rc<BTreeSet<String>>` on `Env` — a flat name-gate. **Set** at 3 call
  boundaries (`apply_values` named/lambda; `eval_method_call`/`call_instance_method`)
  + **seed** in `build_env_batches` (5 caps). **Checked** at 8 native sites via
  `has_authority(cap)` → plain `RuntimeError` on refusal (no telemetry/counter).
- Effects are the 8 natives `emit`/`span`/`print`/`writeConsole`/`readLine`/
  `readByte`/`fsWrite`/`fsRead`, each dispatching straight to `env.platform()` /
  `env.emit_metric` / `env.span_*`. `use <-` is pure sugar, **not** an effect.
- **No handler-installation syntax exists** — `uses` is purely a declaration gate.

### ★ Design decision (settle before D2): the handler model — DECIDED: per-operation (2026-07-11)

- **(CHOSEN) Per-operation handlers.** A handler is a *function* that
  dynamically overrides a named effect op. `Env` gains a dynamically-scoped
  `handlers` stack (op-name → handler value), threaded via `with_handler` **like
  `authority` — no `RefCell`/guard; the block's env clone *is* the dynamic extent**.
  An effect native, before the platform call, checks `env.handler_for("emit")`; a
  handler present → call it with the effect's args (its result is the effect's
  result); else the ambient platform (current behavior). Surface:
  `handle emit with f { body }` — inside `body`, `emit(x)` calls `f(x)`. Minimal,
  tree-walkable, delivers redirection (stim modes) *and* attenuation (a refusing
  handler). **Shallow semantics**: dispatch runs the handler with *its own* op popped
  (`env.without_top_handler("emit")`), so a handler can forward by calling the op
  again (goes to the next handler down / ambient) without self-recursion.
- **(Alt) Per-capability contract handlers.** A cap (`Telemetry`) becomes a
  `contract`, a handler an object implementing it, `handle Telemetry with obj { … }`
  dispatching each op to `obj.emit`/`obj.span`. More Java-shaped, reuses method
  dispatch, but heavier (a handler must implement the whole cap) and couples effects
  to contracts. A later sugar over per-op, once a cap has many ops.

`uses` stays the **gate**; handlers redirect *where an allowed effect goes*.
Attenuation = a refusing handler, or `without Cap { body }` dropping authority for the
extent (D4).

### Steps

**D1 — Spanned structured effect row — ✅ DONE (2026-07-11).**
`uses: Vec<String>` → `Vec<Effect>` (`ast::Effect { name: String, span: Span }`), on
`Item::Func` + `Method`. `Effect`'s `PartialEq`/`Debug` ignore/forward the span (same
metadata treatment as `Expr`) → **zero snapshot churn**; only the one `uses` assert
test updated to compare names. `parse_uses` captures each cap's span (`current_span()`
before `expect_ident`). Consumers that need names-only extract `.name`: `register_items`
→ `ClosureData.uses`, the two method-authority sites, `lower_item_to_core`/
`to_core_method` → `CoreItem`/`CoreMethod` (runtime authority stays `Vec<String>`), and
`bridge::manifest_of_main`'s `needs`. Declaration spans now live on the AST for D4's
refusal messages. Test: `f() uses Telemetry` → span `9..18`. **581 green, clippy clean.**

**D2 — Handler stack + effect dispatch (mechanism, no syntax) — ✅ DONE (2026-07-11).**
`Env` gained `handlers: Rc<Vec<(String, Value)>>` (dynamically-scoped op→value stack,
threaded through *all* constructors like `source` — preserved, not reset at
boundaries, so handlers are dynamic) + `with_handler(op, value)` / `handler_for(op)` /
`without_top_handler(op)`. New `interp::perform_effect(op, args, env, ambient)`:
dispatches to `handler_for(op)` (running it under `without_top_handler(op)` for shallow
forwarding) else runs `ambient`. All 8 effect natives (`emit`/`span`/`print`/
`readLine`/`readByte`/`writeConsole`/`fsWrite`/`readFile`) wrap their platform call in
it, *after* the authority gate (so `uses` still gates; a handler redirects an allowed
effect). No handler installed → identical → 583 green. Handler dynamic scoping works
because `apply_values` builds the call env from the passed env's `globals_only()`,
which carries `handlers`. Test: a handler intercepts `emit("x",1)` and re-emits
`emit("wrapped",1)`, which shallow-forwards to the ambient sink (no self-recursion).
**583 lib + 29 integration green, clippy clean.**

**D3 — `handle` surface syntax + installation.**
New keyword `handle`; parse `handle <op> with <expr> { body }` → a surface AST node
lowering to: eval the handler value, eval `body` under `env.with_handler(op, value)`.
- RED: `handle emit with (n, v) -> record(n, v) { emit("x", 1) }` runs the handler,
  not the ambient emit; an `emit` *outside* the block still hits the sink.
- GREEN: lexer keyword + parser node + lowering/eval arm.

**D4 — Attenuation + spanned unhandled-effect fault.**
`without Cap { body }` (drop authority for the extent) and/or a refusing handler; an
effect with neither authority nor handler faults **spanned**, citing the perform site
(reusing the diagnostics work). Optionally fold the `uses` declaration span (D1) into
the message.
- RED: inside `without Telemetry { emit(…) }` the `emit` faults with a span at the
  call; the same `emit` outside succeeds.
- GREEN: the attenuation construct + spanned refusal.

### Non-goals (Phase D)
- Multi-shot / resumable continuations (VM).
- Effect *inference* (effects are declared, not inferred).
- Per-capability contract handlers (the Alt) until a cap grows many ops.

**Acceptance**: an unhandled effect faults (spanned); a block-scoped handler
redirects/attenuates effects in its extent; `emit`/`span`/`use <-` keep working; 580+
green.

### ★ Decision point (after Phase D)
**stim on the tree-walk core, or continue into the bytecode VM (+ types + GC)
first?** Decide with the **Phase-B5 leak finding** in hand and the felt experience
of the rebuilt core. Because stim is a Stitch *program* on the now-stable IR/effect
interface, the VM can later replace the tree-walker *underneath it* without a
rewrite — so this is a "when," not a "whether." Update [stim-v1](stim-v1.md) here.

---

## Phase F — Cleanups (independent; drop in anytime)

Low-risk, no ordering dependency on A–D.
- **Natives declared in their module namespace** — retire the flat `NATIVES` table
  + hand-maintained `BUILTIN_MODULE_SPECS` map (`interp.rs:86,316`); adding
  `Str.slice` touched three places.
- **Lex interpolations once** — kill the re-lex/re-parse of `StrPart::Expr(String)`
  (`lexer.rs:17`, `parser.rs:166`); nested token groups carry spans into `{…}`.
- **Paren lookahead** — replace `parens_then_arrow`'s unbounded forward scan
  (`parser.rs:386`, O(n²)) with checkpoint/backtrack.

---

## Parallel track — static type checker (VM-independent)

A checker is a static pass over the **core IR** (Phase C); it does **not** require
the bytecode VM (confirmed with the user). The type annotations are already
parsed-but-unchecked (`ast.rs:126`). Useful and important — becomes buildable once
Phase C exists, and can proceed in parallel with D/F/stim. Generics ride on the
type system (downstream of it), not on the VM. *(Its own plan when started.)*

## Deferred to the bytecode-VM milestone

- **GC** (item 6 of the review) — `Rc`→GC-handle values; the collector is additive
  behind the VM, and the VM is where cycle reclamation earns its keep.
- **The bytecode VM** — compiles the Phase-C core IR; Phase B is its execution-loop
  seed. Generics + the richer (multi-shot resumable) effect machinery live here.

## Pre-PR Quality Gate (each phase)

1. Mutation testing (`mutation-testing` skill) on the phase's Rust — **fix the
   `xtask mutants` package list to include `-p stitch`** (currently omitted; found
   during stim Step 1.1).
2. Refactoring assessment (`refactoring` skill).
3. `cargo xtask clippy` + full `stitch` suite green.

## Cross-refs & knock-ons

- [redesign review](../docs/redesign-reviews/stitch-tokenizer-parser-interpreter.md) — the source.
- [stim-v1](stim-v1.md) — **paused pending Phase A+B**; its native-trampoline
  driver may become a Stitch loop once B4 lands; stim-vs-VM decided at the Phase-D
  decision point.
- [stitch mutation testing](../docs/stitch-mutation-testing-design.md) — its fuel
  cap is Phase B2.
- [release-build perf](../docs/stitch-mutation-testing-design.md) — a release build
  compounds interpreter throughput (mutation tester + stim); orthogonal to this plan.

---
*Delete this file when the plan is complete. If `plans/` is empty, delete the directory.*
