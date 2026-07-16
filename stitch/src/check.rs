//! The Stitch type checker: a **bidirectional**, **gradual** static analysis over
//! the Core IR (`core_ir`). It reports spanned type errors without blocking
//! evaluation — unannotated code stays dynamic via [`Ty::Dyn`], which is
//! *consistent* with every type, so a sound check raises no false positives on
//! today's (largely unannotated) programs.
//!
//! Bidirectional means every expression is handled in one of two modes:
//! **synthesize** — read a type off it bottom-up (a literal, a typed variable) —
//! or **check** — verify it against an expected type (a body against its `-> Int`).
//! This module grows one mode/construct per step; see `plans/stitch-type-system.md`.

#[allow(clippy::wildcard_imports, reason = "alloc prelude for no_std")]
use crate::prelude::*;

use alloc::collections::{BTreeMap, BTreeSet};

use crate::ast::{BinOp, Field, Param, Pattern, Type};
use crate::core_ir::{
    CoreArg, CoreExpr, CoreExprKind, CoreItem, CoreMatchArm, CoreStmt, CoreStrSegment,
};
use crate::lexer::Span;
use crate::source::{SourceId, SourceMap};

/// The types of the names in scope while checking a body — currently the
/// function's parameters. Grows to hold `let`-bindings and the receiver later.
pub type TyEnv = BTreeMap<String, Ty>;

/// A Stitch type. Primitives are canonical variants; declared types are `Named`.
/// `Dyn` is the gradual unknown — the type of unannotated / not-yet-known code,
/// *consistent* with every other type so it never triggers a spurious error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ty {
    Int,
    Float,
    Bool,
    Str,
    Unit,
    /// A declared type applied to zero or more arguments: `Point`, `Maybe<Int>`.
    Named { name: String, args: Vec<Ty> },
    /// A function type `(params) -> ret`.
    Func { params: Vec<Ty>, ret: Box<Ty> },
    /// An anonymous product `(A, B, …)`.
    Tuple(Vec<Ty>),
    /// The self-type `@` — resolved to the receiver's own type (a later stage).
    SelfTy,
    /// The gradual unknown; consistent with every type in both directions.
    Dyn,
}

/// How serious a diagnostic is. An **error** is a real mistake (unsafe / wrong);
/// a **warning** is advisory (e.g. an over-broad `uses` declaration). Both are
/// gradual — reported, never blocking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

/// A type diagnostic: a message, the source span it should be reported at, and
/// its severity (rendered later through the `SourceMap`, like a runtime `Fault`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeError {
    pub message: String,
    pub span: Span,
    pub severity: Severity,
}

impl TypeError {
    /// A hard error diagnostic.
    #[must_use]
    fn error(message: String, span: Span) -> TypeError {
        TypeError { message, span, severity: Severity::Error }
    }

    /// An advisory warning diagnostic.
    #[must_use]
    fn warning(message: String, span: Span) -> TypeError {
        TypeError { message, span, severity: Severity::Warning }
    }

    /// Render this diagnostic as `name:line:col: message` + the offending line and
    /// a caret, resolving `span` against `source` in `sources` — the same
    /// presentation as a runtime `Fault`.
    #[must_use]
    pub fn render(&self, sources: &SourceMap, source: SourceId) -> String {
        sources.render(source, self.span, &self.message)
    }
}

/// A declared constructor: which type it builds, that type's generic parameters
/// (in order), and its fields.
struct Ctor {
    type_name: String,
    generics: Vec<String>,
    fields: Vec<FieldTy>,
}

/// One constructor field: its label (`None` when positional), its type, and —
/// when the field's declared type is exactly one of the type's generic
/// parameters (e.g. `Some(T)`) — the index of that parameter, so a construction
/// can solve it from the argument's type.
struct FieldTy {
    label: Option<String>,
    ty: Ty,
    generic: Option<usize>,
}

/// A declared function's signature: its parameter types (in order), return type,
/// and the capabilities it declares it `uses` (propagated to callers by C2).
struct FnSig {
    params: Vec<Ty>,
    ret: Ty,
    uses: Vec<String>,
}

/// The checking context: the program's declared constructors and function
/// signatures (shared across all bodies) plus the types of the names in scope
/// (parameters, for now).
struct Ctx<'a> {
    ctors: &'a BTreeMap<String, Ctor>,
    funcs: &'a BTreeMap<String, FnSig>,
    /// Each declared type → the contracts it conforms to (`on T : C`), for
    /// subtyping: a `T` value is accepted where a conformed-to `C` is expected.
    conformances: &'a BTreeMap<String, BTreeSet<String>>,
    /// Each `sum` type → its variant names, for `match` exhaustiveness.
    sums: &'a BTreeMap<String, Vec<String>>,
    /// The type of `@` in scope: the receiver's `Named` type inside an `on`
    /// method, `SelfTy` in a `contract` default, `Dyn` at top level.
    self_ty: Ty,
    locals: TyEnv,
}

/// Synthesize the type of `expr` bottom-up, pushing any type errors found within
/// it (e.g. a bad constructor argument) into `errors`. Anything not yet
/// understood is [`Ty::Dyn`] — the gradual default — so the checker stays
/// sound-by-omission as constructs are added step by step.
fn synth(expr: &CoreExpr, ctx: &Ctx, errors: &mut Vec<TypeError>) -> Ty {
    match &expr.kind {
        CoreExprKind::Int(_) => Ty::Int,
        CoreExprKind::Float(_) => Ty::Float,
        CoreExprKind::Bool(_) => Ty::Bool,
        // A string literal is `Str` whatever its interpolations synthesize to.
        CoreExprKind::Str(_) => Ty::Str,
        // `()` — the empty tuple — is the unit type.
        CoreExprKind::Tuple(elems) if elems.is_empty() => Ty::Unit,
        // A variable's type comes from the environment (a parameter, for now);
        // names the checker isn't tracking (globals, other functions) are `Dyn`.
        CoreExprKind::Var(name) => ctx.locals.get(name).cloned().unwrap_or(Ty::Dyn),
        // `@` — the receiver, whose type the context carries.
        CoreExprKind::SelfRef => ctx.self_ty.clone(),
        CoreExprKind::Field { object, name } => synth_field(object, name, ctx, errors),
        CoreExprKind::Call { callee, args } => synth_call(callee, args, ctx, errors),
        CoreExprKind::Binary { op, left, right } => {
            synth_binary(*op, left, right, expr.span, ctx, errors)
        }
        CoreExprKind::Match { subject, arms } => synth_match(subject, arms, expr.span, ctx, errors),
        // Everything else is not yet understood: stay gradual (sound-by-omission).
        _ => Ty::Dyn,
    }
}

/// Synthesize a `match`: synthesize the subject and every guard/arm body (for
/// nested errors), and — when the subject has a known `sum` type — require the
/// arms to cover every variant. The match's own type is `Dyn` for now (joining
/// the arm types is a later refinement).
fn synth_match(
    subject: &CoreExpr,
    arms: &[CoreMatchArm],
    span: Span,
    ctx: &Ctx,
    errors: &mut Vec<TypeError>,
) -> Ty {
    let subject_ty = synth(subject, ctx, errors);
    check_exhaustive(&subject_ty, arms, span, ctx, errors);
    for arm in arms {
        if let Some(guard) = &arm.guard {
            synth(guard, ctx, errors);
        }
        synth(&arm.body, ctx, errors);
    }
    Ty::Dyn
}

/// Require a `match` over a known `sum` to cover every variant (or carry an
/// unguarded catch-all). A missing variant is a spanned error naming it. A
/// non-sum or unknown subject type is not checked (gradual).
fn check_exhaustive(
    subject_ty: &Ty,
    arms: &[CoreMatchArm],
    span: Span,
    ctx: &Ctx,
    errors: &mut Vec<TypeError>,
) {
    let Ty::Named { name, .. } = subject_ty else {
        return;
    };
    let Some(variants) = ctx.sums.get(name) else {
        return;
    };
    let mut covered = BTreeSet::new();
    let mut has_catch_all = false;
    for arm in arms {
        // A guarded arm may not fire, so it can't be counted toward coverage.
        if arm.guard.is_none() {
            has_catch_all |= pattern_coverage(&arm.pattern, &mut covered);
        }
    }
    if has_catch_all {
        return;
    }
    let missing: Vec<&str> =
        variants.iter().map(String::as_str).filter(|v| !covered.contains(*v)).collect();
    if !missing.is_empty() {
        errors.push(TypeError::error(
            format!("non-exhaustive match: missing {}", missing.join(", ")),
            span,
        ));
    }
}

/// Record which variants a pattern covers into `covered`, returning whether it is
/// an unguarded catch-all (matches any value). Constructor patterns cover their
/// variant; `_`/bindings catch all; `|` alternatives combine.
fn pattern_coverage(pattern: &Pattern, covered: &mut BTreeSet<String>) -> bool {
    match pattern {
        Pattern::Wildcard | Pattern::Binding(_) => true,
        Pattern::Constructor { name, .. } => {
            covered.insert(name.clone());
            false
        }
        Pattern::Or(alternatives) => {
            // Record every alternative's variants; the Or catches all if any does.
            let mut catch_all = false;
            for alt in alternatives {
                catch_all = pattern_coverage(alt, covered) || catch_all;
            }
            catch_all
        }
        _ => false,
    }
}

/// Synthesize a binary operation: synthesize both operands (collecting any nested
/// errors), then type the operator. An operator that can't apply to the operand
/// types is one error at `span`; a `Dyn` operand is gradual (never an error).
fn synth_binary(
    op: BinOp,
    left: &CoreExpr,
    right: &CoreExpr,
    span: Span,
    ctx: &Ctx,
    errors: &mut Vec<TypeError>,
) -> Ty {
    let l = synth(left, ctx, errors);
    let r = synth(right, ctx, errors);
    if let Some(ty) = binop_type(op, &l, &r) {
        return ty;
    }
    errors.push(TypeError::error(
        format!("operator `{op:?}` cannot apply to `{l:?}` and `{r:?}`"),
        span,
    ));
    Ty::Dyn
}

/// The result type of `op` on operand types `l`/`r`, or `None` when the operator
/// can't apply (a type error). Mirrors the runtime rules in `ops::eval_binary`.
fn binop_type(op: BinOp, l: &Ty, r: &Ty) -> Option<Ty> {
    match op {
        // `+` is numeric addition or string concatenation.
        BinOp::Add => numeric_or_str(l, r),
        BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem => numeric(l, r),
        // Equality is defined between same-kind values only; the result is Bool.
        BinOp::Eq | BinOp::Ne => same_value_kind(l, r).then_some(Ty::Bool),
        BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => orderable(l, r).then_some(Ty::Bool),
        BinOp::And | BinOp::Or => boolish(l, r).then_some(Ty::Bool),
        // Pipes and ranges aren't typed as binary operators yet: gradual.
        BinOp::Pipe | BinOp::CrossPipe | BinOp::Range | BinOp::RangeIncl => Some(Ty::Dyn),
    }
}

/// Numeric arithmetic: both `Int` → `Int`, both `Float` → `Float`; a `Dyn`
/// operand is gradual (`Dyn`); any other pairing is a type error.
fn numeric(l: &Ty, r: &Ty) -> Option<Ty> {
    match (l, r) {
        (Ty::Dyn, _) | (_, Ty::Dyn) => Some(Ty::Dyn),
        (Ty::Int, Ty::Int) => Some(Ty::Int),
        (Ty::Float, Ty::Float) => Some(Ty::Float),
        _ => None,
    }
}

/// `+`: numeric addition, or `Str + Str` string concatenation.
fn numeric_or_str(l: &Ty, r: &Ty) -> Option<Ty> {
    if let (Ty::Str, Ty::Str) = (l, r) {
        return Some(Ty::Str);
    }
    numeric(l, r)
}

/// Whether both operands are of one orderable kind (`Int`/`Float`/`Str`, matching)
/// — or `Dyn`. Ordering comparisons need comparable, same-kind operands.
fn orderable(l: &Ty, r: &Ty) -> bool {
    let ord = |t: &Ty| matches!(t, Ty::Int | Ty::Float | Ty::Str | Ty::Dyn);
    ord(l) && ord(r) && consistent(l, r)
}

/// Whether both operands are booleans (or `Dyn`) — the domain of `and`/`or`.
fn boolish(l: &Ty, r: &Ty) -> bool {
    let is_bool = |t: &Ty| matches!(t, Ty::Bool | Ty::Dyn);
    is_bool(l) && is_bool(r)
}

/// Whether `l` and `r` are the same *value kind* for `==`/`!=` — i.e. would share
/// a `Value` discriminant at runtime (which is where equality is defined). `Ty`'s
/// variant discriminants line up with `Value`'s: each primitive is distinct, and
/// all `Named` (all `Tuple`, all `Func`) collapse to one — so `Circle == Rect` is
/// same-kind, but `1 == 1.0` is not. `Dyn`/`SelfTy` are unknown, hence never an
/// error.
fn same_value_kind(l: &Ty, r: &Ty) -> bool {
    matches!(l, Ty::Dyn | Ty::SelfTy)
        || matches!(r, Ty::Dyn | Ty::SelfTy)
        || core::mem::discriminant(l) == core::mem::discriminant(r)
}

/// Synthesize a call. A call to a declared constructor checks each argument
/// against its field type and yields the constructed `Named` type; other callees
/// aren't typed yet (`Dyn`) — function-call checking arrives in a later step.
fn synth_call(
    callee: &CoreExpr,
    args: &[CoreArg],
    ctx: &Ctx,
    errors: &mut Vec<TypeError>,
) -> Ty {
    let CoreExprKind::Var(name) = &callee.kind else {
        return Ty::Dyn;
    };
    if let Some(ctor) = ctx.ctors.get(name) {
        // Solve the type's generic parameters from the argument types (unsolved
        // ones stay `Dyn` — gradual).
        let mut solved = vec![Ty::Dyn; ctor.generics.len()];
        for (i, arg) in args.iter().enumerate() {
            // Labelled args match by label; positional args by position.
            let field = match &arg.label {
                Some(label) => {
                    ctor.fields.iter().find(|f| f.label.as_deref() == Some(label.as_str()))
                }
                None => ctor.fields.get(i),
            };
            if let Some(field) = field {
                let arg_ty = check(&arg.value, &field.ty, ctx, errors);
                if let Some(g) = field.generic {
                    solved[g] = arg_ty;
                }
            }
        }
        return Ty::Named { name: ctor.type_name.clone(), args: solved };
    }
    if let Some(sig) = ctx.funcs.get(name) {
        // Check each argument against its parameter type (positional); the call's
        // type is the function's declared return.
        for (i, arg) in args.iter().enumerate() {
            if let Some(param) = sig.params.get(i) {
                check(&arg.value, param, ctx, errors);
            }
        }
        return sig.ret.clone();
    }
    Ty::Dyn
}

/// Synthesize a field access `object.name`. When `object` has a declared product
/// type, the field's declared type is looked up (a `prod`'s constructor is keyed
/// by the type name); otherwise — a sum, a builtin, an unknown field — it's `Dyn`.
fn synth_field(object: &CoreExpr, name: &str, ctx: &Ctx, errors: &mut Vec<TypeError>) -> Ty {
    let Ty::Named { name: type_name, .. } = synth(object, ctx, errors) else {
        return Ty::Dyn;
    };
    let Some(ctor) = ctx.ctors.get(&type_name) else {
        return Ty::Dyn;
    };
    ctor
        .fields
        .iter()
        .find(|f| f.label.as_deref() == Some(name))
        .map_or(Ty::Dyn, |f| f.ty.clone())
}

/// Check `expr` against an `expected` type, pushing a mismatch error (at the
/// expression's span) when its synthesized type is inconsistent. The simplest
/// bidirectional rule — synthesize then subsume; expression-directed checking
/// rules (e.g. a lambda against a function type) arrive with later constructs.
fn check(expr: &CoreExpr, expected: &Ty, ctx: &Ctx, errors: &mut Vec<TypeError>) -> Ty {
    let got = synth(expr, ctx, errors);
    if !assignable(&got, expected, ctx.conformances) {
        errors.push(TypeError::error(
            format!("type mismatch: expected `{expected:?}`, found `{got:?}`"),
            expr.span,
        ));
    }
    got
}

/// Whether a value of type `got` can be used where `expected` is wanted:
/// *consistent* (gradual/equal) **or** a subtype of it (contract conformance).
/// Assignability is directional — a `Circle` is a `Drawable`, not vice versa.
#[must_use]
fn assignable(got: &Ty, expected: &Ty, conformances: &BTreeMap<String, BTreeSet<String>>) -> bool {
    consistent(got, expected) || subtype(got, expected, conformances)
}

/// Whether `sub` is a nominal subtype of `sup`: a declared type conforms to the
/// expected contract (`on Sub : Sup`).
#[must_use]
fn subtype(sub: &Ty, sup: &Ty, conformances: &BTreeMap<String, BTreeSet<String>>) -> bool {
    let (Ty::Named { name: sub, .. }, Ty::Named { name: sup, .. }) = (sub, sup) else {
        return false;
    };
    conformances.get(sub).is_some_and(|contracts| contracts.contains(sup))
}

/// Whether two types are *consistent* (gradual `~`): `Dyn` matches anything in
/// either direction. `Named` types match by name with *gradual* arguments — one
/// side's missing (empty) argument list means "unknown", compatible with any;
/// equal-arity argument lists must be pairwise-consistent. Everything else uses
/// structural equality (derived on [`Ty`]). Subtyping is layered on by
/// [`assignable`].
#[must_use]
fn consistent(a: &Ty, b: &Ty) -> bool {
    if matches!(a, Ty::Dyn) || matches!(b, Ty::Dyn) {
        return true;
    }
    match (a, b) {
        (Ty::Named { name: na, args: aa }, Ty::Named { name: nb, args: ab }) => {
            na == nb && args_consistent(aa, ab)
        }
        _ => a == b,
    }
}

/// The gradual argument rule for `Named` types: an empty (unknown) list on either
/// side is compatible; otherwise the arities must match and align pairwise.
#[must_use]
fn args_consistent(aa: &[Ty], ab: &[Ty]) -> bool {
    aa.is_empty()
        || ab.is_empty()
        || (aa.len() == ab.len() && aa.iter().zip(ab).all(|(x, y)| consistent(x, y)))
}

/// Convert a surface type annotation into a [`Ty`], canonicalising the primitive
/// names and resolving names of `types` (the program's declared `prod`/`sum`/
/// `contract`) to nominal `Named` types. Anything else — unknown names (generics,
/// builtins, typos) and function/tuple annotations — stays `Dyn` (gradual).
#[must_use]
fn ty_of_annotation(ann: &Type, types: &BTreeSet<String>) -> Ty {
    match ann {
        Type::Name { name, args } => match name.as_str() {
            "Int" => Ty::Int,
            "Float" => Ty::Float,
            "Bool" => Ty::Bool,
            "Str" => Ty::Str,
            other if types.contains(other) => Ty::Named {
                name: name.clone(),
                args: args.iter().map(|a| ty_of_annotation(a, types)).collect(),
            },
            _ => Ty::Dyn,
        },
        _ => Ty::Dyn,
    }
}

/// Each declared type → the set of contracts it conforms to, from every
/// `on Type : Contract` block.
fn collect_conformances(items: &[CoreItem]) -> BTreeMap<String, BTreeSet<String>> {
    let mut conformances: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for item in items {
        if let CoreItem::On {
            target: Type::Name { name: type_name, .. },
            contract: Some(Type::Name { name: contract_name, .. }),
            ..
        } = item
        {
            conformances.entry(type_name.clone()).or_default().insert(contract_name.clone());
        }
    }
    conformances
}

/// Whether a type annotation mentions the self-type `@` anywhere (directly or
/// nested in a type argument, function, or tuple).
fn contains_self_type(ann: &Type) -> bool {
    match ann {
        Type::SelfType => true,
        Type::Name { args, .. } => args.iter().any(contains_self_type),
        Type::Func { param, ret } => contains_self_type(param) || contains_self_type(ret),
        Type::Tuple(items) => items.iter().any(contains_self_type),
    }
}

/// Each `sum` type → its variant names, in declaration order.
fn collect_sums(items: &[CoreItem]) -> BTreeMap<String, Vec<String>> {
    items
        .iter()
        .filter_map(|item| match item {
            CoreItem::Sum { name, variants, .. } => {
                Some((name.clone(), variants.iter().map(|v| v.name.clone()).collect()))
            }
            _ => None,
        })
        .collect()
}

/// The names of every declared type — `prod`, `sum`, `contract`.
fn collect_type_names(items: &[CoreItem]) -> BTreeSet<String> {
    items
        .iter()
        .filter_map(|item| match item {
            CoreItem::Prod { name, .. }
            | CoreItem::Sum { name, .. }
            | CoreItem::Contract { name, .. } => Some(name.clone()),
            _ => None,
        })
        .collect()
}

/// The field types of a constructor, in declaration order. A field whose type is
/// exactly one of the enclosing type's `generics` is recorded as that parameter
/// and typed `Dyn` (a parameter accepts any argument); the construction solves it.
fn field_tys(fields: &[Field], types: &BTreeSet<String>, generics: &[String]) -> Vec<FieldTy> {
    fields
        .iter()
        .map(|f| {
            let generic = match &f.ty {
                Type::Name { name, args } if args.is_empty() => {
                    generics.iter().position(|g| g == name)
                }
                _ => None,
            };
            let ty = if generic.is_some() { Ty::Dyn } else { ty_of_annotation(&f.ty, types) };
            FieldTy { label: f.name.clone(), ty, generic }
        })
        .collect()
}

/// Index every declared constructor by name: a `prod`'s single constructor and
/// each `sum` variant, mapped to its type name + field types.
fn collect_ctors(items: &[CoreItem], types: &BTreeSet<String>) -> BTreeMap<String, Ctor> {
    let mut ctors = BTreeMap::new();
    for item in items {
        match item {
            CoreItem::Prod { name, generics, fields, .. } => {
                ctors.insert(
                    name.clone(),
                    Ctor {
                        type_name: name.clone(),
                        generics: generics.clone(),
                        fields: field_tys(fields, types, generics),
                    },
                );
            }
            CoreItem::Sum { name, generics, variants, .. } => {
                for variant in variants {
                    ctors.insert(
                        variant.name.clone(),
                        Ctor {
                            type_name: name.clone(),
                            generics: generics.clone(),
                            fields: field_tys(&variant.fields, types, generics),
                        },
                    );
                }
            }
            _ => {}
        }
    }
    ctors
}

/// Index every declared function by name → its parameter and return types
/// (unannotated slots → `Dyn`).
fn collect_funcs(items: &[CoreItem], types: &BTreeSet<String>) -> BTreeMap<String, FnSig> {
    let mut funcs = BTreeMap::new();
    for item in items {
        if let CoreItem::Func { name, params, ret, uses, .. } = item {
            let params = params
                .iter()
                .map(|p| p.ty.as_ref().map_or(Ty::Dyn, |t| ty_of_annotation(t, types)))
                .collect();
            let ret = ret.as_ref().map_or(Ty::Dyn, |t| ty_of_annotation(t, types));
            funcs.insert(name.clone(), FnSig { params, ret, uses: uses.clone() });
        }
    }
    funcs
}

/// Type-check a lowered program, collecting every type error. Each function's
/// body is checked against its declared return type (`Dyn` — hence unchecked —
/// when the return is unannotated), against the program's declared constructors
/// and function signatures.
#[must_use]
pub fn check_program(items: &[CoreItem]) -> Vec<TypeError> {
    let world = World::build(items);
    let mut errors = Vec::new();
    for item in items {
        match item {
            CoreItem::Func { name, params, ret, uses, body, .. } => {
                // Gate `@`: the self-type is meaningless in a top-level function
                // (no receiver). Methods carry a receiver, so they're exempt.
                if signature_mentions_self(params, ret.as_ref()) {
                    errors.push(TypeError::error(
                        format!(
                            "`@` (self-type) is only valid in a method; `{name}` has no receiver"
                        ),
                        body.span,
                    ));
                }
                world.check_callable(params, ret.as_ref(), uses, body, Ty::Dyn, &mut errors);
            }
            // `on Type { … }` method bodies check with `@` = the receiver's type.
            CoreItem::On { target, methods, .. } => {
                let self_ty = ty_of_annotation(target, &world.types);
                for method in methods {
                    if let Some(body) = &method.body {
                        world.check_callable(&method.params, method.ret.as_ref(), &method.uses, body, self_ty.clone(), &mut errors);
                    }
                }
            }
            // `contract` default methods check with `@` = the abstract self.
            CoreItem::Contract { methods, .. } => {
                for method in methods {
                    if let Some(body) = &method.body {
                        world.check_callable(&method.params, method.ret.as_ref(), &method.uses, body, Ty::SelfTy, &mut errors);
                    }
                }
            }
            _ => {}
        }
    }
    errors
}

/// Type-check a single expression against a program's declarations — the REPL's
/// per-line entry. `items` supplies the constructors/functions/types in scope;
/// the expression is synthesized (with `@` and locals empty) and its errors
/// collected.
#[must_use]
pub fn check_expr(expr: &CoreExpr, items: &[CoreItem]) -> Vec<TypeError> {
    let world = World::build(items);
    let ctx = world.ctx(Ty::Dyn, TyEnv::new());
    let mut errors = Vec::new();
    synth(expr, &ctx, &mut errors);
    errors
}

/// The capability an effect-native requires, or `None` for any other name.
/// Mirrors the runtime authority gate in `natives.rs` — keep them in sync.
fn native_cap(name: &str) -> Option<&'static str> {
    match name {
        "emit" | "span" => Some("Telemetry"),
        "print" | "writeConsole" => Some("ConsoleOut"),
        "readLine" | "readByte" => Some("ConsoleIn"),
        "fsWrite" => Some("FsWrite"),
        "readFile" => Some("FsRead"),
        _ => None,
    }
}

/// Walk a body's effects flow-sensitively. Every capability the body exercises is
/// recorded in `required` (an effect-native needs its cap; a called user function
/// propagates its declared `uses` — C1/C2; a user function shadowing a native
/// wins). And — C4 — an effect whose (declared) capability is *withheld* by an
/// enclosing `without Cap { … }` is reported: `dropped` is the set of caps the
/// enclosing `without`s have attenuated away.
fn walk_effects(
    expr: &CoreExpr,
    funcs: &BTreeMap<String, FnSig>,
    declared: &BTreeSet<String>,
    dropped: &BTreeSet<String>,
    required: &mut BTreeMap<String, Span>,
    errors: &mut Vec<TypeError>,
) {
    if let CoreExprKind::Call { callee, .. } = &expr.kind
        && let CoreExprKind::Var(name) = &callee.kind
    {
        let caps: Vec<String> = funcs.get(name).map_or_else(
            || native_cap(name).map(str::to_string).into_iter().collect(),
            |sig| sig.uses.clone(),
        );
        for cap in caps {
            // C4: a declared capability withheld by an enclosing `without` here.
            if dropped.contains(&cap) && declared.contains(&cap) {
                errors.push(TypeError::error(
                    format!("performs an effect that needs `uses {cap}`, but it is withheld here by `without`"),
                    expr.span,
                ));
            }
            required.entry(cap).or_insert(expr.span);
        }
    }
    // `without Cap { body }` attenuates `Cap` for the body's extent; every other
    // node passes `dropped` through unchanged.
    if let CoreExprKind::Without { cap, body } = &expr.kind {
        let mut inner = dropped.clone();
        inner.insert(cap.clone());
        walk_effects(body, funcs, declared, &inner, required, errors);
        return;
    }
    for child in child_exprs(expr) {
        walk_effects(child, funcs, declared, dropped, required, errors);
    }
}

/// The immediate sub-expressions of `expr` (for a uniform traversal). `Without`'s
/// body is *excluded* — its caller handles it specially (attenuated scope).
fn child_exprs(expr: &CoreExpr) -> Vec<&CoreExpr> {
    match &expr.kind {
        // Leaves — and `Without`, whose body the caller walks under an attenuated
        // `dropped`, so it is *not* an ordinary child here.
        CoreExprKind::Int(_)
        | CoreExprKind::Float(_)
        | CoreExprKind::Bool(_)
        | CoreExprKind::Var(_)
        | CoreExprKind::SelfRef
        | CoreExprKind::Without { .. } => Vec::new(),
        CoreExprKind::Spread(e)
        | CoreExprKind::Unary { operand: e, .. }
        | CoreExprKind::Try(e)
        | CoreExprKind::Field { object: e, .. }
        | CoreExprKind::SafeField { object: e, .. } => vec![e],
        CoreExprKind::Lambda { body, .. } => vec![body],
        CoreExprKind::Binary { left, right, .. }
        | CoreExprKind::Index { object: left, index: right } => vec![left, right],
        CoreExprKind::Call { callee, args } => {
            let mut cs = vec![callee.as_ref()];
            cs.extend(args.iter().map(|a| &a.value));
            cs
        }
        CoreExprKind::Range { start, end, .. } => {
            start.iter().chain(end.iter()).map(Box::as_ref).collect()
        }
        CoreExprKind::If { cond, then, els } => vec![cond, then, els],
        CoreExprKind::Tuple(elems) | CoreExprKind::List(elems) => elems.iter().collect(),
        CoreExprKind::Map(entries) => entries.iter().flat_map(|(k, v)| [k, v]).collect(),
        CoreExprKind::Str(segments) => segments
            .iter()
            .filter_map(|s| match s {
                CoreStrSegment::Interp(e) => Some(e.as_ref()),
                CoreStrSegment::Lit(_) => None,
            })
            .collect(),
        CoreExprKind::Block { stmts, result } => {
            let mut cs: Vec<&CoreExpr> = stmts
                .iter()
                .flat_map(|s| match s {
                    CoreStmt::Let { value, .. } | CoreStmt::Expr(value) => vec![value],
                    CoreStmt::Assign { target, value } => vec![target, value],
                })
                .collect();
            cs.extend(result.as_deref());
            cs
        }
        CoreExprKind::Match { subject, arms } => {
            let mut cs = vec![subject.as_ref()];
            for arm in arms {
                cs.extend(arm.guard.as_ref());
                cs.push(&arm.body);
            }
            cs
        }
        CoreExprKind::Handle { handler, body, .. } => vec![handler, body],
    }
}

/// Whether a callable's signature mentions the self-type `@` in any parameter or
/// its return.
fn signature_mentions_self(params: &[Param], ret: Option<&Type>) -> bool {
    params.iter().filter_map(|p| p.ty.as_ref()).any(contains_self_type)
        || ret.is_some_and(contains_self_type)
}

/// The program-wide declarations, shared while checking every body.
struct World {
    ctors: BTreeMap<String, Ctor>,
    funcs: BTreeMap<String, FnSig>,
    conformances: BTreeMap<String, BTreeSet<String>>,
    sums: BTreeMap<String, Vec<String>>,
    types: BTreeSet<String>,
}

impl World {
    /// Index a program's declarations once, for reuse across every body.
    fn build(items: &[CoreItem]) -> World {
        let types = collect_type_names(items);
        World {
            ctors: collect_ctors(items, &types),
            funcs: collect_funcs(items, &types),
            conformances: collect_conformances(items),
            sums: collect_sums(items),
            types,
        }
    }

    /// A checking context over this world with the given `@` type and locals.
    fn ctx(&self, self_ty: Ty, locals: TyEnv) -> Ctx<'_> {
        Ctx {
            ctors: &self.ctors,
            funcs: &self.funcs,
            conformances: &self.conformances,
            sums: &self.sums,
            self_ty,
            locals,
        }
    }

    /// Check one callable's body against its declared return type (with parameters
    /// bound and `@` bound to `self_ty`), and require each effect it performs to be
    /// covered by its declared `uses` capabilities.
    fn check_callable(
        &self,
        params: &[Param],
        ret: Option<&Type>,
        uses: &[String],
        body: &CoreExpr,
        self_ty: Ty,
        errors: &mut Vec<TypeError>,
    ) {
        let locals: TyEnv = params
            .iter()
            .map(|p| (p.name.clone(), p.ty.as_ref().map_or(Ty::Dyn, |t| ty_of_annotation(t, &self.types))))
            .collect();
        let ctx = self.ctx(self_ty, locals);
        let expected = ret.map_or(Ty::Dyn, |t| ty_of_annotation(t, &self.types));
        check(body, &expected, &ctx, errors);
        // Effect check. `walk_effects` collects into `required` the caps the body
        // exercises — direct effect natives (C1) and, transitively, the `uses` of
        // functions it calls (C2) — and flags effects withheld by a `without` (C4).
        let declared: BTreeSet<String> = uses.iter().cloned().collect();
        let mut required = BTreeMap::new();
        walk_effects(body, &self.funcs, &declared, &BTreeSet::new(), &mut required, errors);
        // Forward (C1/C2): a required cap that isn't declared is an error.
        for (cap, span) in &required {
            if !declared.contains(cap) {
                errors.push(TypeError::error(
                    format!("performs an effect that needs `uses {cap}`, which is not declared"),
                    *span,
                ));
            }
        }
        // Reverse (C3): a declared cap that's never exercised is a warning — the
        // least-authority lint. (Conservative: `required` under-approximates, so
        // effects reached only via methods/higher-order calls could over-warn.)
        for cap in &declared {
            if !required.contains_key(cap) {
                errors.push(TypeError::warning(
                    format!("declares `uses {cap}` but never uses it"),
                    body.span,
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Ctx, Ty, TyEnv};
    use alloc::collections::BTreeMap;

    /// Lower a literal source expression to a `CoreExpr` for synthesis.
    fn core(src: &str) -> crate::core_ir::CoreExpr {
        crate::lower::lower_expr_to_core(&crate::parser::parse(src).expect("parses"))
    }

    /// Synthesize the type of a source expression in an empty context.
    fn ty(src: &str) -> Ty {
        let ctors = BTreeMap::new();
        let funcs = BTreeMap::new();
        let conformances = BTreeMap::new();
        let sums = BTreeMap::new();
        let ctx = Ctx {
            ctors: &ctors,
            funcs: &funcs,
            conformances: &conformances,
            sums: &sums,
            self_ty: Ty::Dyn,
            locals: TyEnv::new(),
        };
        super::synth(&core(src), &ctx, &mut Vec::new())
    }

    #[test]
    fn the_real_stitch_programs_type_check_clean() {
        // A regression guard: the actual `.st` code — the prelude stdlib and the
        // stim editor FSM — must stay free of (false-positive) type warnings, so
        // the gradual checker never flags correct real-world code.
        let prelude = crate::lower::lower_items_to_core(&crate::interp::prelude_items());
        assert_eq!(super::check_program(&prelude), Vec::new(), "prelude type-checks clean");
        let stim = crate::lower::lower_items_to_core(
            &crate::parser::parse_program(include_str!("../../fs-image/stim/stim.st"))
                .expect("stim parses"),
        );
        assert_eq!(super::check_program(&stim), Vec::new(), "stim type-checks clean");
    }

    #[test]
    fn literals_synthesize_their_canonical_type() {
        assert_eq!(ty("4"), Ty::Int);
        assert_eq!(ty("4.0"), Ty::Float);
        assert_eq!(ty("true"), Ty::Bool);
        assert_eq!(ty(r#""hi""#), Ty::Str);
        assert_eq!(ty("()"), Ty::Unit);
    }

    #[test]
    fn a_non_empty_tuple_is_not_unit_and_stays_gradual_for_now() {
        // Only the *empty* tuple is `Unit`; a populated tuple is not yet
        // synthesized structurally, so it stays `Dyn`. Pins the `is_empty()`
        // guard (a mutant that drops it would mis-type `(1, 2)` as `Unit`).
        assert_eq!(ty("(1, 2)"), Ty::Dyn);
    }

    /// Type-check a whole program, returning its type errors.
    fn errors(src: &str) -> Vec<super::TypeError> {
        let items = crate::lower::lower_items_to_core(
            &crate::parser::parse_program(src).expect("parses"),
        );
        super::check_program(&items)
    }

    #[test]
    fn a_function_body_is_checked_against_its_declared_return_type() {
        // A body whose synthesized type is inconsistent with the declared return
        // is one error, reported at the body expression.
        let src = r#"f() -> Int = "x""#;
        let errs = errors(src);
        assert_eq!(errs.len(), 1, "one return-type mismatch, got {errs:?}");
        assert_eq!(
            errs[0].span.start,
            src.find(r#""x""#).expect("has the body"),
            "the error is at the body: {errs:?}"
        );
        // A matching return type is clean (structural equality).
        assert!(errors(r#"f() -> Str = "x""#).is_empty(), "Str body matches Str return");
        // An unannotated return is `Dyn` — consistent with anything (the `b` side).
        assert!(errors(r#"f() = "x""#).is_empty(), "no annotation → no check");
        // A body whose type is unknown (`Dyn`, the `a` side) is also consistent.
        assert!(errors("f() -> Int = (1, 2)").is_empty(), "Dyn body → no check");
    }

    #[test]
    fn the_return_check_spans_each_primitive_and_stays_gradual_for_unknown_annotations() {
        // Every primitive return is checked...
        assert_eq!(errors(r#"f() -> Float = "x""#).len(), 1, "Float ≠ Str");
        assert_eq!(errors(r#"f() -> Bool = "x""#).len(), 1, "Bool ≠ Str");
        assert_eq!(errors("f() -> Str = 1").len(), 1, "Str ≠ Int");
        // ...but an annotation the checker doesn't track yet is gradual (`Dyn`):
        // an unknown type *name*, and a non-name (tuple) annotation.
        assert!(errors(r#"f() -> Foo = "x""#).is_empty(), "unknown type name → Dyn → clean");
        assert!(errors(r#"f() -> () = "x""#).is_empty(), "non-name annotation → Dyn → clean");
    }

    #[test]
    fn a_parameter_annotation_flows_into_the_body() {
        // A parameter used as the body carries its declared type, so it is checked
        // against the return type.
        let src = "f(x: Str) -> Int = x";
        let errs = errors(src);
        assert_eq!(errs.len(), 1, "param `x: Str` ≠ `Int` return, got {errs:?}");
        assert_eq!(
            errs[0].span.start,
            src.rfind('x').expect("the body `x`"),
            "the error is at the body reference: {errs:?}"
        );
        // A matching parameter type is clean.
        assert!(errors("f(x: Int) -> Int = x").is_empty(), "param Int matches Int return");
        // An unannotated parameter is `Dyn` — consistent with anything.
        assert!(errors("f(x) -> Int = x").is_empty(), "unannotated param → Dyn → clean");
    }

    #[test]
    fn a_constructor_argument_is_checked_against_its_field_type() {
        // `y: Int` receives a `Str` — one error, at the offending argument.
        let src = r#"prod Point(x: Int, y: Int)  f() = Point(1, "x")"#;
        let errs = errors(src);
        assert_eq!(errs.len(), 1, "field `y: Int` got `Str`, got {errs:?}");
        assert_eq!(
            errs[0].span.start,
            src.find(r#""x""#).expect("the bad arg"),
            "the error is at the argument: {errs:?}"
        );
        // Well-typed arguments are clean.
        assert!(
            errors("prod Point(x: Int, y: Int)  f() = Point(1, 2)").is_empty(),
            "matching arg types are clean"
        );
        // A `Dyn` argument (an unannotated param) is consistent with any field.
        assert!(
            errors("prod Point(x: Int, y: Int)  f(a) = Point(1, a)").is_empty(),
            "Dyn argument → clean"
        );
        // Labelled arguments bind by field *name*, not position: with distinct
        // field types, a positional-only checker would mis-match these.
        assert!(
            errors(r#"prod P(x: Int, y: Str)  f() = P(y: "s", x: 1)"#).is_empty(),
            "labelled args bind by name, not position"
        );
        assert_eq!(
            errors(r#"prod P(x: Int, y: Str)  f() = P(x: "no", y: "s")"#).len(),
            1,
            "labelled `x: Int` got Str"
        );
    }

    #[test]
    fn a_sum_variant_constructor_argument_is_checked() {
        // Each `sum` variant is a constructor too: its arguments are checked
        // against the variant's field types.
        let src = "sum Shape = Circle(Int) | Rect(Int, Int)  f() = Circle(\"x\")";
        let errs = errors(src);
        assert_eq!(errs.len(), 1, "Circle(Int) got Str, got {errs:?}");
        assert_eq!(errs[0].span.start, src.find(r#""x""#).expect("the bad arg"));
        assert!(
            errors("sum Shape = Circle(Int) | Rect(Int, Int)  f() = Rect(1, 2)").is_empty(),
            "matching variant args are clean"
        );
    }

    #[test]
    fn a_function_call_checks_arguments_and_synthesizes_the_return_type() {
        // A wrong argument type is one error, at the argument.
        let src = r#"g(x: Int) -> Str = "y"  f() = g("no")"#;
        let errs = errors(src);
        assert_eq!(errs.len(), 1, "g expects Int, got Str, got {errs:?}");
        assert_eq!(errs[0].span.start, src.find(r#""no""#).expect("the bad arg"));
        // A correct argument is clean.
        assert!(errors(r#"g(x: Int) -> Str = "y"  f() = g(1)"#).is_empty(), "correct arg clean");
        // The call synthesizes the return type: `g(1)` is `Str`, so using it where
        // an `Int` is expected is an error (proves the result type flows out).
        assert_eq!(
            errors(r#"g(x: Int) -> Str = "y"  f() -> Int = g(1)"#).len(),
            1,
            "call result Str ≠ Int return"
        );
        // An unknown callee is `Dyn` — its call is unchecked.
        assert!(errors("f() = unknownFn(1)").is_empty(), "unknown callee → Dyn → clean");
    }

    #[test]
    fn binary_operators_check_operand_types_and_synthesize_a_result() {
        // Arithmetic on mismatched kinds is one error, at the operation.
        let src = "f() = 1 + true";
        let errs = errors(src);
        assert_eq!(errs.len(), 1, "Int + Bool, got {errs:?}");
        assert_eq!(errs[0].span.start, src.find("1 + true").expect("the op"));
        // Well-typed arithmetic and string concatenation are clean.
        assert!(errors("f() = 1 + 2").is_empty(), "Int + Int");
        assert!(errors("f() = 1.0 * 2.0").is_empty(), "Float * Float");
        assert!(errors(r#"f() = "a" + "b""#).is_empty(), "Str + Str concat");
        // The result type flows out: `1 + 2` is Int, `1 < 2` is Bool.
        assert_eq!(errors("f() -> Str = 1 + 2").len(), 1, "Int result ≠ Str return");
        assert_eq!(errors("f() -> Int = 1 < 2").len(), 1, "Bool result ≠ Int return");
        // Ordering across kinds is an error; same orderable kinds are clean.
        assert_eq!(errors(r#"f() = 1 < "x""#).len(), 1, "Int < Str");
        assert!(errors("f() = 1 < 2").is_empty(), "Int < Int is a clean comparison");
        // Logic operators need booleans (clean when both are) and yield Bool.
        assert_eq!(errors("f() = 1 and true").len(), 1, "Int `and` Bool");
        assert!(errors("f() = true and false").is_empty(), "Bool `and` Bool is clean");
        assert_eq!(errors("f() -> Int = true or false").len(), 1, "Bool result ≠ Int return");
        // Equality yields Bool regardless of the (same-kind) operands.
        assert_eq!(errors("f() -> Int = 1 == 2").len(), 1, "== result Bool ≠ Int return");
        // A `Dyn` operand suppresses the error (gradual).
        assert!(errors("f(a) = a + 1").is_empty(), "Dyn operand → no error");
    }

    #[test]
    fn equality_requires_same_kind_operands() {
        // Same-kind equality is clean.
        assert!(errors("f() = 1 == 2").is_empty(), "Int == Int");
        assert!(errors(r#"f() = "a" != "b""#).is_empty(), "Str != Str");
        // Cross-kind equality is an error (matching the runtime discriminant rule).
        assert_eq!(errors(r#"f() = 1 == "x""#).len(), 1, "Int == Str");
        assert_eq!(errors("f() = 1 == 2.0").len(), 1, "Int == Float");
        // Two *different* declared types share a value-kind (both heap data), which
        // the runtime allows (returns false) — so the checker accepts it too.
        assert!(
            errors("prod A(x: Int)  prod B(x: Int)  f() = A(1) == B(1)").is_empty(),
            "Named == Named is a same-kind comparison"
        );
        // A `Dyn` operand suppresses the check.
        assert!(errors("f(a) = a == 1").is_empty(), "Dyn == Int → clean");
    }

    #[test]
    fn the_self_type_is_rejected_in_a_top_level_function_signature() {
        // `@` (self-type) names the receiver's type — meaningless in a top-level
        // function, which has no receiver.
        assert_eq!(errors("foo() -> @ = foo()").len(), 1, "@ return outside a method");
        assert_eq!(errors("foo(x: @) = x").len(), 1, "@ param outside a method");
        assert_eq!(errors("foo() -> Maybe<@> = foo()").len(), 1, "@ nested in a type argument");
        assert_eq!(errors("foo() -> @ -> Int = foo()").len(), 1, "@ on one side of a function type");
        // A signature without `@` is clean.
        assert!(errors("foo(x: Int) -> Int = x").is_empty(), "no @ → clean");
        // Inside an `on` method, `@` is allowed (methods aren't gated here).
        assert!(errors("prod P(n: Int)  on P { dup() -> @ = @ }").is_empty(), "@ in a method is fine");
    }

    #[test]
    fn a_declared_type_name_in_an_annotation_is_checked() {
        // A user `prod`/`sum`/`contract` name resolves to a nominal type, so a
        // return annotation of that type is checked.
        assert_eq!(
            errors(r#"prod Point(x: Int)  f() -> Point = "x""#).len(),
            1,
            "Str body ≠ Point return"
        );
        assert!(
            errors("prod Point(x: Int)  f() -> Point = Point(1)").is_empty(),
            "a Point body matches a Point return"
        );
        // An unknown type name stays gradual (`Dyn`).
        assert!(
            errors(r#"f() -> Unknown = "x""#).is_empty(),
            "unknown type name → Dyn → clean"
        );
    }

    #[test]
    fn an_effect_without_its_declared_capability_is_an_error() {
        // `emit` needs `Telemetry`; a function that emits without declaring it errors.
        let errs = errors(r#"f() = emit("x", 1)"#);
        assert_eq!(errs.len(), 1, "emit needs `uses Telemetry`, got {errs:?}");
        assert!(errs[0].message.contains("Telemetry"), "names the cap: {errs:?}");
        // Declaring the capability is clean.
        assert!(errors(r#"f() uses Telemetry = emit("x", 1)"#).is_empty(), "declared → clean");
        // Each effect-native needs its own capability.
        assert_eq!(errors(r#"f() = print("hi")"#).len(), 1, "print needs `uses ConsoleOut`");
        assert_eq!(errors("f() = readLine()").len(), 1, "readLine needs `uses ConsoleIn`");
        assert_eq!(errors(r#"f() = fsWrite(0, "x")"#).len(), 1, "fsWrite needs `uses FsWrite`");
        assert_eq!(errors("f() = readFile(0)").len(), 1, "readFile needs `uses FsRead`");
        // The effect is found nested inside a block.
        assert_eq!(errors("f() = { emit(\"x\", 1) }").len(), 1, "effect nested in a block");
        // A method body is checked the same way.
        assert_eq!(
            errors(r#"prod P(n: Int)  on P { m() = emit("x", 1) }"#).len(),
            1,
            "a method effect needs its cap too"
        );
    }

    #[test]
    fn an_effect_withheld_by_without_is_an_error_even_when_declared() {
        // `f` declares Telemetry but drops it in the block, then emits there — a
        // compile-time error, matching the runtime refusal.
        let errs = errors(r#"f() uses Telemetry = without Telemetry { emit("x", 1) }"#);
        assert_eq!(errs.len(), 1, "emit is withheld inside `without Telemetry`, got {errs:?}");
        assert!(errs[0].message.contains("withheld"), "explains the withholding: {errs:?}");
        // Emitting outside the `without` is fine (Telemetry is available).
        assert!(errors(r#"f() uses Telemetry = emit("x", 1)"#).is_empty(), "not withheld → clean");
        // Dropping a *different* capability doesn't withhold Telemetry.
        assert!(
            errors(r#"f() uses Telemetry = without ConsoleOut { emit("x", 1) }"#).is_empty(),
            "dropping ConsoleOut doesn't withhold Telemetry"
        );
    }

    #[test]
    fn a_declared_but_unused_capability_is_a_warning() {
        // Declaring a capability that's never exercised is a (non-fatal) warning —
        // the least-authority lint: your `uses` should be exactly what you need.
        let errs = errors("f() uses Telemetry = 1");
        assert_eq!(errs.len(), 1, "one unused-cap warning, got {errs:?}");
        assert_eq!(errs[0].severity, super::Severity::Warning, "it warns, not errors");
        assert!(errs[0].message.contains("Telemetry"), "names the cap: {errs:?}");
        // Exercising the capability — directly or via a call — is clean.
        assert!(errors(r#"f() uses Telemetry = emit("x", 1)"#).is_empty(), "used directly → clean");
        assert!(
            errors(r#"g() uses Telemetry = emit("x", 1)  f() uses Telemetry = g()"#).is_empty(),
            "used transitively via a call → clean"
        );
    }

    #[test]
    fn calling_a_capability_using_function_propagates_the_requirement() {
        // `g` calls `f`, which uses Telemetry, so `g` must declare Telemetry too.
        let bad = r#"f() uses Telemetry = emit("x", 1)  g() = f()"#;
        assert_eq!(errors(bad).len(), 1, "g calls f (uses Telemetry) undeclared, got {:?}", errors(bad));
        // Declaring it up the chain is clean.
        assert!(
            errors(r#"f() uses Telemetry = emit("x", 1)  g() uses Telemetry = f()"#).is_empty(),
            "g declares Telemetry → clean"
        );
        // Calling a pure function requires nothing.
        assert!(errors("f() = 1  g() = f()").is_empty(), "calling a pure function → clean");
    }

    #[test]
    fn a_method_body_is_checked_against_its_return_type() {
        // An `on` method's body is checked like a function's.
        assert_eq!(
            errors(r#"prod P(n: Int)  on P { m() -> Int = "x" }"#).len(),
            1,
            "Str method body ≠ Int return"
        );
        assert!(
            errors(r#"prod P(n: Int)  on P { m() -> Str = "x" }"#).is_empty(),
            "a matching method body is clean"
        );
        // A `contract` default method's body is checked too.
        assert_eq!(
            errors(r#"contract C { greet() -> Int = "x" }"#).len(),
            1,
            "default method Str ≠ Int return"
        );
    }

    #[test]
    fn the_receiver_and_its_fields_are_typed_in_a_method() {
        // `@n` reads the receiver's field `n` (Int).
        assert_eq!(
            errors("prod P(n: Int)  on P { m() -> Str = @n }").len(),
            1,
            "@n is Int, not the declared Str return"
        );
        assert!(
            errors("prod P(n: Int)  on P { m() -> Int = @n }").is_empty(),
            "@n Int matches an Int return"
        );
        // `@` itself is the receiver's type.
        assert!(errors("prod P(n: Int)  on P { me() -> P = @ }").is_empty(), "@ is a P");
        assert_eq!(errors("prod P(n: Int)  on P { me() -> Str = @ }").len(), 1, "@ is a P, not Str");
    }

    #[test]
    fn a_match_over_a_sum_must_cover_every_variant() {
        // A `match` over a sum-typed subject that omits a variant is an error.
        let src = "sum Shape = Circle(Int) | Rect(Int, Int)  area(s: Shape) = match s { Circle(r) => r }";
        let errs = errors(src);
        assert_eq!(errs.len(), 1, "Rect is not covered, got {errs:?}");
        assert!(errs[0].message.contains("Rect"), "the error names the missing variant: {errs:?}");
        // Covering every variant is clean.
        assert!(
            errors("sum Shape = Circle(Int) | Rect(Int, Int)  area(s: Shape) = match s { Circle(r) => r  Rect(w, h) => w }").is_empty(),
            "all variants covered"
        );
        // A wildcard (or an `A | B` alternative) covers the rest.
        assert!(
            errors("sum Shape = Circle(Int) | Rect(Int, Int)  area(s: Shape) = match s { Circle(r) => r  _ => 0 }").is_empty(),
            "wildcard covers the rest"
        );
        // A match on an unknown (unannotated) subject stays gradual.
        assert!(errors("f(s) = match s { Circle(r) => r }").is_empty(), "Dyn subject → no check");
        // An `A | B` alternative covers both variants; a bare `_` inside an Or
        // catches all the rest.
        assert!(
            errors("sum T = A | B | C  f(t: T) = match t { A | B => 0  C => 1 }").is_empty(),
            "an Or covers each of its alternatives"
        );
        assert!(
            errors("sum T = A | B | C  f(t: T) = match t { A | _ => 0 }").is_empty(),
            "an Or containing `_` catches all"
        );
        assert_eq!(
            errors("sum T = A | B | C  f(t: T) = match t { A | B => 0 }").len(),
            1,
            "C is still missing behind an Or of A and B"
        );
    }

    #[test]
    fn a_generic_type_argument_is_checked() {
        // `Box<Int>` and `Box<Str>` are distinct instantiations: passing one where
        // the other is expected is an error.
        let bad = "prod Box<T>(v: T)  sink(b: Box<Str>) = 0  g(x: Box<Int>) = sink(x)";
        assert_eq!(errors(bad).len(), 1, "Box<Int> ≠ Box<Str>, got {:?}", errors(bad));
        // Matching instantiations are clean.
        assert!(
            errors("prod Box<T>(v: T)  sink(b: Box<Int>) = 0  g(x: Box<Int>) = sink(x)").is_empty(),
            "Box<Int> matches Box<Int>"
        );
        // A bare (un-parameterized) generic type is gradual on its arguments.
        assert!(
            errors("prod Box<T>(v: T)  sink(b: Box) = 0  g(x: Box<Int>) = sink(x)").is_empty(),
            "Box ~ Box<Int> (unknown args are gradual)"
        );
    }

    #[test]
    fn a_generic_constructor_infers_its_type_argument() {
        // `Wrap(5)` fills `Opt`'s `T` with `Int`, so it's an `Opt<Int>` — passing it
        // where `Opt<Str>` is expected is an error.
        let bad = "sum Opt<T> = Wrap(T) | Empty  sink(o: Opt<Str>) = 0  f() = sink(Wrap(5))";
        assert_eq!(errors(bad).len(), 1, "Wrap(5) is Opt<Int> ≠ Opt<Str>, got {:?}", errors(bad));
        // A matching instantiation is clean.
        assert!(
            errors("sum Opt<T> = Wrap(T) | Empty  sink(o: Opt<Int>) = 0  f() = sink(Wrap(5))").is_empty(),
            "Wrap(5) matches Opt<Int>"
        );
        // A nullary variant leaves the parameter unknown (gradual).
        assert!(
            errors("sum Opt<T> = Wrap(T) | Empty  sink(o: Opt<Str>) = 0  f() = sink(Empty)").is_empty(),
            "Empty is Opt<?> — gradual against any Opt"
        );
        // Only a *bare* parameter is a solvable slot: `T` applied to arguments
        // (`T<Int>`) is not, so its field stays gradual and solves nothing.
        assert!(
            errors("sum Weird<T> = V(T<Int>)  sink(w: Weird<Str>) = 0  f() = sink(V(5))").is_empty(),
            "T<Int> is not a bare-parameter slot"
        );
    }

    #[test]
    fn a_conforming_type_is_accepted_where_its_contract_is_expected() {
        // `Circle` conforms to `Drawable` (`on Circle : Drawable`), so a `Circle`
        // is accepted where a `Drawable` parameter is expected — subtyping.
        let ok = concat!(
            "contract Drawable { draw() -> Str }  ",
            "prod Circle(r: Int)  ",
            "on Circle : Drawable { draw() -> Str = \"o\" }  ",
            "render(d: Drawable) -> Str = \"x\"  ",
            "f() = render(Circle(1))",
        );
        assert!(errors(ok).is_empty(), "a Circle is a Drawable: {:?}", errors(ok));
        // Without the conformance, a `Circle` is not a `Drawable`.
        let bad = concat!(
            "contract Drawable { draw() -> Str }  ",
            "prod Circle(r: Int)  ",
            "render(d: Drawable) -> Str = \"x\"  ",
            "f() = render(Circle(1))",
        );
        assert_eq!(errors(bad).len(), 1, "a non-conforming Circle is rejected");
    }
}
