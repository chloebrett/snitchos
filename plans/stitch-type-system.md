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
- **Stage 2 — exhaustive `match` — ✅ DONE (2026-07-13).** `synth(Match)` →
  `synth_match` (synth subject + guards + arm bodies) + `check_exhaustive`: when the
  subject has a known `sum` type (`collect_sums` → `Ctx.sums`), the arms must cover every
  variant or carry an unguarded catch-all, else a spanned error names the missing
  variants. `pattern_coverage` handles `Constructor` (covers its variant), `Wildcard`/
  `Binding` (catch-all), and `Or` (combines alternatives); guarded arms don't count
  toward coverage. Match result type is `Dyn` for now (joining arm types is a
  refinement). 608 lib + all integration green, **0 false positives** (existing matches
  are exhaustive). Mutation 98/113 caught, 0 survivors. Pattern-binding variable types
  still `Dyn` (a `Circle(r)` arm's `r` is unknown).
- **Stage 3 — `@` self-type.** **Gating ✅ DONE (2026-07-12)**: `contains_self_type` +
  a check in `check_program` — `@` (`Type::SelfType`, incl. nested in a type arg /
  function / tuple) in a top-level `Func` signature is an error (naturally scoped:
  `check_program` only walks `Func`, so `On`/`Contract` method `@`s are untouched).
  Error at the body span (annotations are unspanned; a precise `@` span would need
  parser support — follow-up). **Meaning ✅ DONE (2026-07-12)** with method-body
  checking: `Ctx.self_ty` holds the receiver type, `synth(SelfRef)` returns it, and
  `synth_field` looks up `@field` against the receiver `prod`'s declared field types.

- **Method-body checking ✅ DONE (2026-07-12).** `check_program` refactored around a
  `World` (shared decls) + `check_callable`; now checks `on Type { … }` method bodies
  (`@` = the receiver `Named` type) and `contract` default-method bodies (`@` = `SelfTy`)
  against their return types, in addition to `Func` bodies. 607 lib + all integration
  green, **zero false positives** on real `on`-block programs. Mutation 79/93 caught,
  0 survivors. (Sum-variant field access + `@method()` sibling-call return types still
  gradual/`Dyn`.)
- **Stage 4 — generics + local inference.** Broken down below (see *Stage 4 — Generics
  breakdown*).
- **Stage 5 — contract subtyping — ✅ DONE (2026-07-12).** Two cycles: **(A)** declared
  `prod`/`sum`/`contract` names in annotations resolve to `Named` (via `collect_type_names`
  threaded into `ty_of_annotation`; unknown names stay `Dyn`) — so `f() -> Point = "x"`
  is now caught; **(B)** `check` uses `assignable(got, expected)` = `consistent` **or**
  `subtype`, where `subtype` consults `collect_conformances` (every `on T : C`). A
  `Circle` is accepted where a `Drawable` is expected iff `on Circle : Drawable`;
  directional (not vice-versa). 603 lib + all integration green — **zero false positives**
  from resolving user type names. Mutation 74 → 61 caught / 13 unviable / 0 survivors.
  (Method-body checking — `on`/`contract` methods — still deferred; only `Func` bodies
  are checked.)
- **Stage 6 — capabilities as effects.** Lift `uses` from the runtime gate to a static
  effect check: a body performing an effect must be under a declared/inherited `uses`.
  The headline feature; the runtime gate becomes a backstop.
- **Later** — immutable-key (`Key`/`Hashable`) constraint, `Map`/`Set` key eligibility.

---

## Stage 4 — Generics breakdown (scoped 2026-07-13)

**Pivotal fact:** generic *types* are supported syntactically (`prod P<T>`, `sum S<T>`,
`contract C<T>` carry `generics`; `Maybe<Int>` annotations parse), but generic
*functions/methods* are **not** — `Func`/`Method`/`On` have no `generics` field and
`parse_func` reads `name` then `(` (so `id<T>(x)` is a parse error). That splits generics
into two tracks: **A (generic types)** is checker-only and doable now; **B (generic
functions)** needs front-end (parser + AST) work first.

**Standing constraint:** the prelude is *all* generics (`Maybe`/`Result`/`List`/`Seq`,
generic combinators). Every step must keep `the_real_stitch_programs_type_check_clean`
green — i.e. **no false positives**. The rule of thumb: when a type argument can't be
resolved/inferred, fall back to gradual (`Dyn` / arity-mismatch = compatible), never
error. `Ty::Named.args` is currently *always* empty, so today generic types are matched
by name only — that's the gradual baseline to preserve.

### Track A — generic types (checker-only, no parser work)

- **G1 — Type-argument annotations + gradual-arg consistency.** `ty_of_annotation`
  recurses into `<…>` so `Maybe<Int>` → `Named{Maybe, [Int]}`. `consistent` for `Named`:
  same name; if **both** sides carry args of equal arity, args must be pairwise-consistent;
  if either side has **empty/mismatched-arity** args (unknown), it's gradual-compatible.
  → `f() -> Maybe<Int> = Some(1)` stays clean (the `Some` result has empty args), but
  `Maybe<Int>` vs `Maybe<Str>` is caught. *Small, mechanical; the Track-A prerequisite.*
- **G2 — Generic constructor instantiation.** `Some(5)` → `Maybe<Int>`, `Cons(1, t)` →
  `List<Int>`: a constructor knows its type's `generics` and which field carries which
  param, so unify the argument types against the field types to solve the params and
  yield `Named{Type, [solved…]}`. Unsolved params → `Dyn` arg (gradual). *Inference-lite
  — one level, no unification engine. Medium; first real "solve a type variable".*

### Track B — generic functions (needs front-end first, then inference)

- **G3 — Generic function/method syntax (parser + AST, no checker logic).** Add
  `generics: Vec<String>` to `Func`, `Method` (and thread through `CoreItem`/lowering);
  parse `foo<T>(…)`. Optionally parse bounds `<T: Drawable>` here (needed by G6) — or
  defer bounds to G6. *Front-end only; unblocks writing generic functions. No type
  errors change.*
- **G4 — Rigid type-params in generic definitions.** With G3's `generics`, checking
  `id<T>(x: T) -> T = x` adds `T` to the in-scope type names so it resolves to a rigid
  param (`Named{T}` suffices — same-name consistency makes `x: T` match `-> T`); a real
  mismatch (`id<T>(x: T) -> Int = x`) is caught. *Checks generic **definitions**; no
  call-site inference. Self-contained given G3.*
- **G5 — Generic function-call inference (the hard part).** `id(5)` → solve `T = Int` →
  result `Int`; `map(f, xs)` → solve `A`, `B`. Introduces flexible inference variables
  (`Ty::Var(u32)`) + a unifier + substitution; instantiate a generic signature with fresh
  vars, unify args against params, apply the solution to the return. Unsolvable → `Dyn`
  (gradual, no false positive). *The unification engine; the biggest, riskiest step.*
- **G6 — Bounds (`T: Drawable`).** At the instantiation site, the solved type must conform
  to the bound (reuse the conformance table); inside a bounded body, `T` is known to
  conform, so contract methods on it are allowed. *Depends on G4/G5 + bound syntax.*

### Sequencing & pause points

Order **G1 → G2** (deliver generic **types**, entirely in `check.rs`, low risk) — a
coherent, shippable milestone on its own. **Reassess before Track B:** G3 is a
front-end detour and G5 is a genuine unification engine (the checker's first). Decide
then whether generic *functions* are worth that depth now, or whether generic *types*
(A) plus the existing gradual fallback for generic functions is enough for a while.
Bounds (G6) only matter once G4/G5 exist.

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
**GREEN**: `TypeError::render(&SourceMap, SourceId)` (same presentation as a runtime `Fault`); `runner::type_check_report` lowers the program, runs `check_program`, and prepends `type error: …` lines to a run's stderr — **advisory only, never changes the exit code or blocks eval** (the gradual report-don't-block default, chosen with the user). Wired into `run_program_source` (single-module path). **REPL + multi-module wired
2026-07-13**: `check_expr(expr, items)` (new entry, via `World::build`/`World::ctx`) checks
each REPL expression → `<repl>:line:col` warnings; `run_module_files` checks each module
(message-only, cross-module refs `Dyn`). REPL *declaration*-line checking still deferred.
**Mutation**: check.rs 38 (29 caught / 9 unviable / 0 survivors); runner `type_check_report` 2/2 caught.
**Done**: 600 lib + all integration green; **zero false positives** on existing programs (gradual `Dyn` held); clippy clean.

### Step 5: binary-operator operands — ✅ DONE (2026-07-12, after 7)
**Acceptance**: `1 + 2 : Int`; `1.0 + 2.0 : Float`; `"a" + "b" : Str`; `1 + true` errors; comparisons/logic yield `Bool`; a `Dyn` operand suppresses the error.
**GREEN**: `synth(Binary)` → `synth_binary` (synth both operands, then `binop_type(op, l, r) -> Option<Ty>`, `None` = a spanned operator error). Mirrors `ops::eval_binary`: `numeric` (Int/Int→Int, Float/Float→Float), `numeric_or_str` (`+` also `Str+Str`), `orderable` (matching Int/Float/Str → Bool), `boolish` (`and`/`or` → Bool); `Eq`/`Ne` → Bool; pipes/ranges → `Dyn`. **Eq/Ne operand-kind check added 2026-07-12** — `same_value_kind` (via `core::mem::discriminant` on `Ty`, whose variants line up with runtime `Value` kinds) errors on `1 == "x"` / `1 == 1.0` but accepts cross-`Named` `A == B` (same heap-data kind, which the runtime allows); `Dyn`/`SelfTy` never error.
**Mutation**: 56 mutants — killed 2 survivors (`orderable`/`boolish` "always false" — needed *clean-context* assertions, since return-annotation tests give 1 error either way); final 43 caught / 13 unviable / 0 survivors.
**Done**: 601 lib green, clippy clean.

## Pre-PR Quality Gate
1. Mutation testing (`cargo xtask mutants -p stitch`, now wired).
2. Refactoring assessment.
3. `cargo xtask clippy` clean; full `stitch` suite + integration green.

---
*Delete when the type system is delivered (or split per-stage as stages land).*
