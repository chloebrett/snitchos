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

use crate::ast::{BinOp, Field, Type};
use crate::core_ir::{CoreArg, CoreExpr, CoreExprKind, CoreItem};
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

/// A type error: a message plus the source span it should be reported at
/// (rendered later through the `SourceMap`, like a runtime `Fault`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeError {
    pub message: String,
    pub span: Span,
}

impl TypeError {
    /// Render this error as `name:line:col: message` + the offending line and a
    /// caret, resolving `span` against `source` in `sources` — the same
    /// presentation as a runtime `Fault`.
    #[must_use]
    pub fn render(&self, sources: &SourceMap, source: SourceId) -> String {
        sources.render(source, self.span, &self.message)
    }
}

/// A declared constructor: which type it builds, and its fields in order.
struct Ctor {
    type_name: String,
    fields: Vec<FieldTy>,
}

/// One constructor field: its label (`None` when positional) and its type.
struct FieldTy {
    label: Option<String>,
    ty: Ty,
}

/// A declared function's signature: its parameter types (in order) and its
/// return type.
struct FnSig {
    params: Vec<Ty>,
    ret: Ty,
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
        CoreExprKind::Call { callee, args } => synth_call(callee, args, ctx, errors),
        CoreExprKind::Binary { op, left, right } => {
            synth_binary(*op, left, right, expr.span, ctx, errors)
        }
        // Everything else is not yet understood: stay gradual (sound-by-omission).
        _ => Ty::Dyn,
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
    errors.push(TypeError {
        message: format!("operator `{op:?}` cannot apply to `{l:?}` and `{r:?}`"),
        span,
    });
    Ty::Dyn
}

/// The result type of `op` on operand types `l`/`r`, or `None` when the operator
/// can't apply (a type error). Mirrors the runtime rules in `ops::eval_binary`.
fn binop_type(op: BinOp, l: &Ty, r: &Ty) -> Option<Ty> {
    match op {
        // `+` is numeric addition or string concatenation.
        BinOp::Add => numeric_or_str(l, r),
        BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem => numeric(l, r),
        // Equality is defined between same-kind values; the result is always Bool.
        // (Operand-kind checking is deferred; the result type is what matters here.)
        BinOp::Eq | BinOp::Ne => Some(Ty::Bool),
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
        for (i, arg) in args.iter().enumerate() {
            // Labelled args match by label; positional args by position.
            let field = match &arg.label {
                Some(label) => {
                    ctor.fields.iter().find(|f| f.label.as_deref() == Some(label.as_str()))
                }
                None => ctor.fields.get(i),
            };
            if let Some(field) = field {
                check(&arg.value, &field.ty, ctx, errors);
            }
        }
        return Ty::Named { name: ctor.type_name.clone(), args: Vec::new() };
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

/// Check `expr` against an `expected` type, pushing a mismatch error (at the
/// expression's span) when its synthesized type is inconsistent. The simplest
/// bidirectional rule — synthesize then subsume; expression-directed checking
/// rules (e.g. a lambda against a function type) arrive with later constructs.
fn check(expr: &CoreExpr, expected: &Ty, ctx: &Ctx, errors: &mut Vec<TypeError>) {
    let got = synth(expr, ctx, errors);
    if !assignable(&got, expected, ctx.conformances) {
        errors.push(TypeError {
            message: format!("type mismatch: expected `{expected:?}`, found `{got:?}`"),
            span: expr.span,
        });
    }
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
/// either direction; otherwise types must be equal. Structural equality (derived
/// on [`Ty`]) covers `Named`/`Tuple`/`Func` for free. Subtyping is layered on top
/// by [`assignable`].
#[must_use]
fn consistent(a: &Ty, b: &Ty) -> bool {
    matches!(a, Ty::Dyn) || matches!(b, Ty::Dyn) || a == b
}

/// Convert a surface type annotation into a [`Ty`], canonicalising the primitive
/// names and resolving names of `types` (the program's declared `prod`/`sum`/
/// `contract`) to nominal `Named` types. Anything else — unknown names (generics,
/// builtins, typos) and function/tuple annotations — stays `Dyn` (gradual).
#[must_use]
fn ty_of_annotation(ann: &Type, types: &BTreeSet<String>) -> Ty {
    match ann {
        Type::Name { name, .. } => match name.as_str() {
            "Int" => Ty::Int,
            "Float" => Ty::Float,
            "Bool" => Ty::Bool,
            "Str" => Ty::Str,
            other if types.contains(other) => Ty::Named { name: name.clone(), args: Vec::new() },
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

/// The field types of a constructor, in declaration order.
fn field_tys(fields: &[Field], types: &BTreeSet<String>) -> Vec<FieldTy> {
    fields
        .iter()
        .map(|f| FieldTy { label: f.name.clone(), ty: ty_of_annotation(&f.ty, types) })
        .collect()
}

/// Index every declared constructor by name: a `prod`'s single constructor and
/// each `sum` variant, mapped to its type name + field types.
fn collect_ctors(items: &[CoreItem], types: &BTreeSet<String>) -> BTreeMap<String, Ctor> {
    let mut ctors = BTreeMap::new();
    for item in items {
        match item {
            CoreItem::Prod { name, fields, .. } => {
                ctors.insert(
                    name.clone(),
                    Ctor { type_name: name.clone(), fields: field_tys(fields, types) },
                );
            }
            CoreItem::Sum { name, variants, .. } => {
                for variant in variants {
                    ctors.insert(
                        variant.name.clone(),
                        Ctor { type_name: name.clone(), fields: field_tys(&variant.fields, types) },
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
        if let CoreItem::Func { name, params, ret, .. } = item {
            let params = params
                .iter()
                .map(|p| p.ty.as_ref().map_or(Ty::Dyn, |t| ty_of_annotation(t, types)))
                .collect();
            let ret = ret.as_ref().map_or(Ty::Dyn, |t| ty_of_annotation(t, types));
            funcs.insert(name.clone(), FnSig { params, ret });
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
    let types = collect_type_names(items);
    let ctors = collect_ctors(items, &types);
    let funcs = collect_funcs(items, &types);
    let conformances = collect_conformances(items);
    let mut errors = Vec::new();
    for item in items {
        if let CoreItem::Func { params, ret, body, .. } = item {
            // Bind each parameter to its declared type (unannotated → `Dyn`).
            let locals: TyEnv = params
                .iter()
                .map(|p| (p.name.clone(), p.ty.as_ref().map_or(Ty::Dyn, |t| ty_of_annotation(t, &types))))
                .collect();
            let ctx = Ctx { ctors: &ctors, funcs: &funcs, conformances: &conformances, locals };
            let expected = ret.as_ref().map_or(Ty::Dyn, |t| ty_of_annotation(t, &types));
            check(body, &expected, &ctx, &mut errors);
        }
    }
    errors
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
        let ctx = Ctx {
            ctors: &ctors,
            funcs: &funcs,
            conformances: &conformances,
            locals: TyEnv::new(),
        };
        super::synth(&core(src), &ctx, &mut Vec::new())
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
