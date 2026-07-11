# Plan: Stitch type system (bidirectional, gradual)

**Status**: Active — Stage 1 (skeleton + nominal checks)
**Track**: parallel to the bytecode VM; a pass over the Phase-C Core IR, VM-independent.
**Design source**: `docs/language-design.md` → *Type system* (worked out, pre-implementation).

## Goal

A static type checker for Stitch: a pass over the Core IR that reports **spanned**
type errors (reusing the C5 `SourceMap`/`Fault` diagnostics), catching nominal
mismatches, non-exhaustive matches, `@` misuse, and — eventually — capability-effect
(`uses`) violations at compile time instead of runtime.

## Architecture decisions (settled 2026-07-11)

1. **Bidirectional** (`synth ⇄ check`), not Hindley-Milner. Fits the language: nominal
   `prod`/`sum`, optional annotations, contract *subtyping* (which HM doesn't do), and
   the `@` self-type. No global unifier needed to start; the check/synth duality is the
   real modern-PLT lesson. Local inference vars (`Ty::Var`) arrive only when generics
   need them (a later stage), not in Stage 1.
2. **Gradual / additive.** The checker is a new pass; **unannotated code stays dynamic**.
   The mechanism is a `Ty::Dyn` that is *consistent* (`~`) with every type in both
   directions (gradual typing's `?` / TypeScript `any`). Unannotated params and
   unknown-type expressions become `Dyn`, so the checker only errors on **concrete**
   mismatches — today's 588 tests + the (largely unannotated) prelude stay green because
   a sound checker raises no false positives on valid dynamic code.
3. **Checks the Core IR.** `CoreItem` carries the type metadata (`Param.ty`, `ret`,
   `Field`, `Variant`, generics) unchanged from the surface AST, and `CoreExpr` carries
   spans (C2/C5). The checker builds a type context from the `CoreItem`s (declared types
   + their fields/variants, function signatures) then checks each function/method body.
4. **Wiring is deferred within Stage 1.** The checker is built + tested as a *standalone
   pass* (`check_program(&[CoreItem]) -> Vec<TypeError>`) first. Wiring it into the run
   path (report errors / gate execution) is the last step of Stage 1, once it is mature
   enough not to false-positive — the safest gradual rollout.

### Core representation (Stage 1)

```rust
enum Ty {
    Int, Float, Bool, Str, Unit,        // canonical primitives
    Named { name: String, args: Vec<Ty> }, // Point, Maybe<Int>, List<Str>
    Func { params: Vec<Ty>, ret: Box<Ty> },
    Tuple(Vec<Ty>),
    SelfTy,                             // @ — resolved to the receiver later
    Dyn,                               // gradual unknown; consistent with all
}
```

- `consistent(a, b)` — the gradual relation: `Dyn ~ anything`; primitives match by
  identity; `Named` by name + pairwise-consistent args; `Func`/`Tuple` structurally.
  (Contract *subtyping* extends this in a later stage; Stage 1 is exact-match + `Dyn`.)
- `ty_of_annotation(&Type) -> Ty` — canonicalises `Int`/`Float`/`Bool`/`Str`/`()` to
  primitives, other `Name`s to `Named`, threads `Func`/`Tuple`/`@`.
- `TypeError { message: String, span: Span }` — rendered via the existing `SourceMap`.

## Staged roadmap (each stage ≈ one PR-sized slice)

- **Stage 1 — skeleton + nominal checks** (this plan's steps). `Ty`, `synth`/`check`,
  `consistent`, spanned `TypeError`; check literals, function param/return vs body,
  constructor arg types, binary-op operand types, and calls. Wire in as a reported pass.
- **Stage 2 — exhaustive `match`.** Over a sum subject, every variant covered (or a
  `_`), else a spanned error naming the missing variants. Needs Stage 1's subject synth.
- **Stage 3 — `@` self-type.** Meaning (`@` = receiver's own type) + gating (parse/type
  error outside an `on`/`contract` method).
- **Stage 4 — generics + local inference.** `Ty::Var`, instantiation of `Maybe<T>` etc.,
  bound checking (`T: Drawable`), the monomorphisation-relevant checks.
- **Stage 5 — contract subtyping.** `render(d: Drawable)` accepts any conforming type;
  `consistent` grows a subtype arm driven by the conformance table.
- **Stage 6 — capabilities as effects.** Lift `uses` from the runtime gate to a static
  effect check: a body performing an effect must be under a declared/inherited `uses`.
  The headline feature; the runtime gate becomes a backstop.
- **Later** — immutable-key (`Key`/`Hashable`) constraint, `Map`/`Set` key eligibility.

---

## Stage 1 steps

Every step follows RED → GREEN → (MUTATE/KILL) → REFACTOR. New module `stitch/src/check.rs`.

### Step 1: `Ty` + synth of literals
**Acceptance**: `synth` of an `Int`/`Float`/`Bool`/`Str`/unit literal returns the
canonical `Ty`; a `TypeError` type exists carrying a message + span.
**RED**: a test that `synth` of a `4` core-expr yields `Ty::Int` (and a string literal `Ty::Str`).
**GREEN**: `Ty` enum, `synth(&CoreExpr, &Ctx) -> Ty` covering literal arms.

### Step 2: `check` + function return vs body — ✅ DONE (2026-07-11)
**Acceptance**: a function `f() -> Int = "x"` reports one error at the body span; `f() -> Str = "x"` reports none; an unannotated `f() = "x"` reports none (return `Dyn`).
**GREEN**: `check(&CoreExpr, expected) -> Option<TypeError>` (synth-then-subsume); `consistent(a, b) = Dyn|Dyn|a==b` — structural equality (derived on `Ty`) covers `Named`/`Tuple`/`Func` for free, so the predicate is complete and clean (subtyping extends it in Stage 5); `ty_of_annotation` canonicalises primitive names, everything else `Dyn` (gradual); `check_program(&[CoreItem])` checks each `Func` body against its declared return (`Dyn` when absent), via `lower_items_to_core`.
**Mutation**: 22 mutants, 18 caught / 4 unviable, 0 survivors — tests cover each primitive arm, both `Dyn` operands, structural match, and the gradual fallbacks.
**Done**: 594 lib green, clippy clean.

### Step 3: params in the type context — ✅ DONE (2026-07-11)
**Acceptance**: `f(x: Int) -> Int = x` clean; `f(x: Str) -> Int = x` errors at the body; `f(x) -> Int = x` clean (param `Dyn`).
**GREEN**: `TyEnv = BTreeMap<String, Ty>`; `synth`/`check` take `&TyEnv`; `synth(Var)` reads it (unknown names → `Dyn`); `check_program` binds each param (`ty_of_annotation`, else `Dyn`) into the env per function body.
**Mutation**: 23 mutants, 19 caught / 4 unviable, 0 survivors.
**Done**: 595 lib green, clippy clean.

### Step 4: constructor argument types — ✅ DONE (2026-07-11)
**Acceptance**: with `prod Point(x: Int, y: Int)`, `Point(1, "x")` errors at the `"x"` arg (`y: Int` got `Str`); `Point(1, 2)` clean; a `Dyn` arg is clean.
**GREEN**: introduced the error-accumulator architecture — `synth`/`check` now take a `&Ctx` (declared constructors + local `TyEnv`) and push into `&mut Vec<TypeError>` (a construction both synthesizes a `Named` type *and* emits arg errors). `collect_ctors` indexes every `prod` constructor + `sum` variant → `(type_name, field_tys)`; `synth_call` checks each arg against its field type (labelled by name, positional by index) and yields the `Named` type. Non-constructor calls stay `Dyn` (function-call checking is Step 6).
**Mutation**: 33 mutants — real Sum-arm survivor killed with a sum-variant test; final run 0 missed (remaining timeouts are load-induced, each provably caught).
**Done**: 597 lib green, clippy clean.

### Step 5: binary-operator operands
**Acceptance**: `1 + 2 : Int`; `1.0 + 2.0 : Float`; `1 + true` errors; string `++`/comparisons per the ops table; a `Dyn` operand suppresses the error.
**RED**: `1 + true` yields one error; `1 + 2` none.
**GREEN**: `synth(Binary)` encodes each operator's operand/result typing (arithmetic, comparison, boolean, concat) against `consistent`.

### Step 6: call argument + result types — ✅ DONE (2026-07-11)
**Acceptance**: calling `f(x: Int) -> Str` as `f("no")` errors at the arg; `f(1)` clean and the call synthesizes `Str`; calling an unknown/`Dyn` callee is clean.
**GREEN**: `FnSig { params, ret }` + `collect_funcs` index every declared function; `Ctx` gained a `funcs` map; `synth_call` grew a second arm — a known function checks each arg against its parameter type (positional) and yields the declared return (`Dyn` for unknown callees). Return-type synthesis proven by `f() -> Int = g(1)` erroring when `g` returns `Str`.
**Mutation**: 36 mutants, 27 caught / 9 unviable, 0 survivors.
**Done**: 598 lib green, clippy clean.

### Step 7: wire the pass in (reported, non-fatal) — ✅ DONE (2026-07-12)
**Acceptance**: a host entry collects type errors and renders them via the `SourceMap`; running a well-typed program is unchanged; the suite + prelude stay green (no false positives).
**GREEN**: `TypeError::render(&SourceMap, SourceId)` (same presentation as a runtime `Fault`); `runner::type_check_report` lowers the program, runs `check_program`, and prepends `type error: …` lines to a run's stderr — **advisory only, never changes the exit code or blocks eval** (the gradual report-don't-block default, chosen with the user). Wired into `run_program_source` (single-module path; REPL + multi-module wiring deferred).
**Mutation**: check.rs 38 (29 caught / 9 unviable / 0 survivors); runner `type_check_report` 2/2 caught.
**Done**: 600 lib + all integration green; **zero false positives** on existing programs (gradual `Dyn` held); clippy clean.

**Note — Step 5 (binary-operator operands) was leapfrogged.** Per the user's 6→7→5
ordering (where "5" = **contract subtyping**, i.e. the roadmap's *Stage 5*), the
plan-step-5 "binary-operator operands" (`1 + true` errors) is **not yet done** —
`synth(Binary)` still falls through to `Dyn`. It remains an open, self-contained step to
pick up whenever.

## Pre-PR Quality Gate
1. Mutation testing (`cargo xtask mutants -p stitch`, now wired).
2. Refactoring assessment.
3. `cargo xtask clippy` clean; full `stitch` suite + integration green.

---
*Delete when the type system is delivered (or split per-stage as stages land).*
