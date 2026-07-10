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

The big structural piece and the true VM prework. **AST node spans AND identifier
interning both land here** (one churn of the exact-tree tests, together with the
reshape) — the resolution pass that lowers surface→core is the natural home for
turning names into interned `Symbol`s/slots, and doing spans + symbols in the same
AST reshape honours the "don't churn the AST twice" rule.

**Phase goal**: the parser emits a faithful, round-trippable **surface AST** (keep
`Placeholder`, a real `SubjectlessMatch`, an `OperatorRef` — no more parse-time
desugars); a **single lowering pass** produces a smaller **core IR**, absorbing
*both* the parse-time desugars and the eval-time `use <-` lowering; `Value::Closure`
holds a **code-ref into the IR + upvalues**, not `body: Expr`; the reified evaluator
evaluates the core IR; runtime `Fault`s carry spans (via the B3 frame stack).

**Current state (2026-07-10, after B and F completion):**
- Parser already emits faithful surface AST — `SubjectlessMatch`, `Placeholder`,
  `OperatorRef` all survive to the AST; goal 1 is already achieved.
- `lower.rs` desugars in place by mutating `Expr` values; produces no separate IR
  type. Current desugars: `SubjectlessMatch` → nested `If`; `OperatorRef` → Lambda;
  `Placeholder` → `Var + Lambda`; `Stmt::Use` → callback-lambda.
- `ClosureData.body: Expr` — owned clone of the body expr; no code-ref sharing.
  Upvalues landed in C4 (2026-07-09).
- No spans on AST nodes; `ParseError` is a bare `String`; `Env` depth is a
  counter only — no frame stack, no per-call location.
- No intern infrastructure in the eval pipeline. 543 tests green.

**Acceptance**: desugaring lives in exactly one pass; tree-walker evaluates the
core IR; runtime faults cite `line:col`; 543+ green.

---

### Step C1: ParseError carries a real span — ✅ DONE (2026-07-10)

`ParseError { message: String, span: Span }` existed but span was always
`Span::default()`. Added `Parser::current_span()` + `Parser::err(msg)` helper;
replaced all 21 `ParseError::new(...)` call sites with `self.err(...)` (or
`parser.err()`/`sub.err()` in free functions). Interpolation sub-parser now
propagates its own span. 10 insta snapshots updated to show real byte offsets.
544 tests green.

---

### Step C2: Define CoreExpr + lowering produces it

**What**: New `src/core.rs` defines `CoreExpr` (and `CoreStmt`, `CoreItem`,
`CorePattern`, `CoreMatchArm`) — the IR the evaluator will consume. CoreExpr is a
strict subset of Expr: same structure but **no** `SubjectlessMatch`, `Placeholder`,
`OperatorRef`, `Spread` (desugared by lowering). Every node carries a `Span`.

Key structural differences from `Expr`:
- `CoreExpr::Var(String)` initially (Symbol arrives in C3 alongside the eval port).
- `CoreExpr::Lambda { params: Vec<String>, body: Rc<CoreExpr> }` — body is `Rc`,
  not `Box`, so multiple closures over the same definition share the IR node.
- No `Stmt::Use` in `CoreStmt` (desugared away).
- `CoreMatchArm` has `CorePattern` and `CoreExpr` body.

**New `lower.rs` API** (additive; old in-place API kept for now):
```rust
pub fn lower_expr(expr: &Expr) -> Result<CoreExpr, LowerError>
pub fn lower_item(item: &Item) -> Result<CoreItem, LowerError>
pub fn lower_items(items: &[Item]) -> Result<Vec<CoreItem>, LowerError>
```
`LowerError` for `Placeholder` in non-call-arg position and other malformed surface
constructs that the parser accepts but lowering rejects.

**TDD order**:
1. RED: tests in `lower.rs` that call `lower_expr` on surface constructs and assert
   the resulting `CoreExpr` shape (one test per desugar: SubjectlessMatch, OperatorRef,
   Placeholder, Use).
2. GREEN: write `core.rs` + `lower_expr`.
3. Existing 543 tests unaffected (they still use old `lower_program` in-place path).

---

### Step C3: Interpreter evaluates CoreExpr + identifier interning

**Why together**: folding interning into the eval-port step means `CoreExpr::Var`
becomes `CoreExpr::Var(Symbol)` in the same pass — one full-tree update, not two.

**What**:

**(a) Symbol table** — new `src/symbol.rs`: `SymbolTable` maps `String → Symbol(u32)`
(insert-or-get, O(log n)). `free_vars` in `lower.rs` switches to `BTreeSet<Symbol>`.
Resolution pass (part of lowering): every `String` name in the lowered IR is
interned through the program's `SymbolTable`; `CoreExpr::Var(Symbol)`,
`CoreExpr::Lambda { params: Vec<Symbol>, body: Rc<CoreExpr> }`, etc.

**(b) Slot-indexed locals in Env** — `Env.locals` becomes `Vec<(Symbol, Rc<RefCell<Value>>, bool)>`
(or a flat `Vec<Slot>` indexed by symbol ID for O(1) local lookup). Globals stay
`BTreeMap<String, Value>` (or keyed by Symbol — decide at implementation time based
on profile).

**(c) `eval_core(expr: &CoreExpr, env: &Env) → Result<Value, RuntimeError>`** — new
evaluator function alongside existing `eval`. Written from scratch matching CoreExpr
arms. `eval_tail_core` handles the trampoline path.

**(d) Wire-up**: `build_env_with_backends` calls `lower_items` → `CoreItem`s and
registers them. `apply_values` calls `eval_core` (not `eval`). Once all tests pass
through the new path, the old `eval(expr: &Expr, ...)` and in-place `lower_program`
are deleted.

**TDD order**:
1. RED: one integration test that runs a Stitch program through `lower_items` +
   `eval_core` directly and checks the result — before the full wire-up.
2. GREEN: implement `eval_core` for all arms, one at a time (test per arm if
   granularity demands; or branch-by-branch against existing test suite).
3. Wire `build_env_with_backends` to use the new path; all 543+ tests go green
   through the new evaluator.
4. Delete old `eval` + in-place `lower_program`.

---

### Step C4: ClosureData.body: Rc\<CoreExpr\> — ✅ DONE (2026-07-09, upvalue capture)

The upvalue + `home_globals: Weak<OnceCell<…>>` design landed. `body: Expr` still
present but this is addressed structurally in C3 (the `eval_core` port makes the
body type switch to `Rc<CoreExpr>` as part of the same change).

---

### Step C5: Frame stack + spanned faults

**What**: Replace `depth: Rc<Cell<u32>>` in `Env` with a
`frames: Rc<RefCell<Vec<CallFrame>>>` where:
```rust
struct CallFrame {
    span: Span,
    name: Option<String>,  // function name if known
}
```
`enter_call` pushes; `CallGuard` drop pops. `RuntimeError::Fault` becomes:
```rust
Fault { message: String, at: Option<Span> }
```
The `at` span comes from the `CoreExpr` node being evaluated when the fault fires.
The frame stack provides the backtrace.

Acceptance: `run("1 / 0")` returns `Fault { message: "division by zero", at: Some(Span { start: 2, end: 7 }) }`.
All existing fault-message tests updated to match the new error type.

---

### Phase C non-goals (deferred to VM milestone)

- Full bytecode compilation of CoreExpr
- Generics / type parameters in CoreExpr
- Multi-shot continuations
- Inlining / dead-code elimination passes

---

## Phase D — Effects: structured `uses` row + runtime handler-stack

**Phase goal**: `uses: Vec<String>` (`ast.rs:32`) → a structured, **spanned effect
row** on the surface AST + IR; `authority: Rc<BTreeSet<String>>` (`env.rs:59`) → a
runtime **handler stack of capability *values*** on the `Interp` (Phase B's home);
performing an effect walks the handler stack; installing a handler scopes it to a
block's dynamic extent. The *membrane* semantics stim's modes-as-authority needs —
no VM required (multi-shot resumable continuations, which would want the VM, are
explicitly out of scope here).

**Acceptance (to detail later)**: an unhandled effect faults (spanned); a
block-scoped handler attenuates effects in its extent; `emit`/`span`/`use <-` keep
working.

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
