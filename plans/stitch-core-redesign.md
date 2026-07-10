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

### Step C2: Surface `Expr` carries per-node spans

**Why first**: lowering operates on `&Expr`, not on tokens — so CoreExpr spans can
only be *real* once the surface `Expr` carries them. This is also the one
unavoidable churn of the exact-tree tests; doing it up front means C3/C4 are born
with real spans and each stays testable.

**What**: wrap AST nodes so each carries a `Span`. Preferred shape (matches the C3
CoreExpr shape, so the mirror is 1:1):
```rust
pub struct Expr { pub kind: ExprKind, pub span: Span }
pub enum ExprKind { Int(i64), Var(String), Binary { … }, … }  // today's Expr variants
```
Parser construction sites set `span` from the token range they consumed (start of
the first token .. end of the last). `Stmt` gets a span too (needed for C5 frames
and `use <-` desugar provenance). `Pattern` spans are **out of scope** for now
(faults cite expression positions, not pattern positions).

**Blast radius**: every `Expr::Foo { … }` literal in `parser.rs`, `lower.rs`,
`interp.rs`, and tests becomes `Expr { kind: ExprKind::Foo { … }, span }`. The
`insta` snapshots of parsed trees churn once (accept the new shape). This is
mechanical but wide — the bulk of C2's effort.

**TDD order**:
1. RED: `parse("  x").unwrap().span` points at byte 2 (the `x`), not 0; a binary
   expr's span covers both operands.
2. GREEN: introduce `ExprKind`, thread spans through parser construction sites.
3. Accept the churned tree snapshots; keep all behavior tests green (544+).

**Interim value**: even before CoreExpr exists, the *current* eval could begin
citing locations — but we don't thread spans into faults yet (that's C5, on the
core IR). C2 is the span-origination substrate.

---

### Step C3: Define CoreExpr + lowering produces it (with real spans)

**What**: New `src/core.rs` defines `CoreExpr { kind: CoreExprKind, span: Span }`
(plus `CoreStmt`, `CoreItem`, `CoreArg`, `CoreStrSegment`, `CoreMatchArm`) — the IR
the evaluator consumes. `CoreExprKind` is `ExprKind` minus the surface-only nodes:
**no** `SubjectlessMatch`, `OperatorRef`, `Placeholder`. `Spread` **stays** (it's
core — `construct()` needs it). `Pattern` is reused unchanged (no surface-only
pattern variants). Names stay `String` (interning deferred — see phase note).

Key structural points:
- `CoreExprKind::Lambda { params: Vec<String>, body: Rc<CoreExpr> }` — `Rc`, not
  `Box`, so a closure captures a shared code-ref instead of deep-cloning the body.
- `CoreItem::Func { …, body: Rc<CoreExpr> }` likewise.
- `CoreStmt` has only `Let`/`Assign`/`Expr` (no `Use` — desugared to a `Call`).
- Every node's `span` is copied from the surface node it lowered from.

**New `lower.rs` API** (additive; old in-place `lower_program` kept until C4):
```rust
pub fn lower_expr_to_core(expr: &Expr) -> CoreExpr
pub fn lower_item_to_core(item: &Item) -> CoreItem
pub fn lower_items_to_core(items: &[Item]) -> Vec<CoreItem>
```
Infallible for now: a `Placeholder` surviving in non-arg position lowers to
`Var("$a")` (faults at eval, as today). A spanned `LowerError` is a later refinement
once we want to reject it at lower-time pointing at the `$`.

**TDD order** (structural tests on the internal IR):
1. RED: `lower_expr_to_core` on a parsed `SubjectlessMatch` → top-level kind is `If`;
   on `OperatorRef(Add)` → a 2-param `Lambda`; on a `use x <- f(1)` block → a `Call`;
   on a lambda → `body` is `Rc<CoreExpr>`; a node's `span` matches its surface origin.
2. GREEN: write `core.rs` + the three `*_to_core` functions.
3. Existing 544 tests unaffected (they still run through the old in-place path).

---

### Step C4: Interpreter evaluates CoreExpr; delete the old path

**What**:

**(a) `eval_core(expr: &CoreExpr, env: &Env) -> Result<Value, RuntimeError>`** — new
evaluator matching `CoreExprKind` arms; `eval_tail_core` for the trampoline. Written
alongside the existing `eval` so tests stay green throughout.

**(b) `ClosureData.body: Expr → Rc<CoreExpr>`** — the code-ref switch lands here (the
former "C4", partly done via upvalue capture 2026-07-09; the body-type change is the
remaining piece). Closure creation `Rc::clone`s the body instead of deep-cloning.

**(c) Wire-up**: `build_env_with_backends` calls `lower_items_to_core` and registers
`CoreItem`s; `apply_values` calls `eval_core`. When all 544+ tests pass through the
new path, **delete** the old `eval(&Expr, …)` and the in-place `lower_program`.

**TDD order**:
1. RED: an integration test running a program through `lower_items_to_core` +
   `eval_core` directly, asserting the result — before full wire-up.
2. GREEN: implement `eval_core` arm by arm against the existing behavior suite.
3. Wire the default path over; all 544+ green through the new evaluator.
4. Delete the old `eval` + in-place `lower_program`; `body: Expr` field is gone.

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

Acceptance: `run("1 / 0")` returns `Fault { message: "division by zero", at: Some(span) }`
where `span` covers the offending division expression (e.g. `Span { start: 0, end: 5 }`
for the whole `1 / 0`, or the operator at `2..3` — pick one convention and test it).
All existing fault-message tests updated to match the new error type.

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
