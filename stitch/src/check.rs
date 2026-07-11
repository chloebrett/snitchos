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

use alloc::collections::BTreeMap;

use crate::ast::Type;
use crate::core_ir::{CoreExpr, CoreExprKind, CoreItem};
use crate::lexer::Span;

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

/// Synthesize the type of `expr` bottom-up. Anything not yet understood is
/// [`Ty::Dyn`] — the gradual default — so the checker stays sound-by-omission
/// as constructs are added step by step.
#[must_use]
pub fn synth(expr: &CoreExpr, env: &TyEnv) -> Ty {
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
        CoreExprKind::Var(name) => env.get(name).cloned().unwrap_or(Ty::Dyn),
        // Everything else is not yet understood: stay gradual (sound-by-omission).
        _ => Ty::Dyn,
    }
}

/// Check `expr` against an `expected` type, returning a mismatch error if the
/// type synthesized for `expr` is inconsistent with `expected`. Step 2 uses the
/// simplest bidirectional rule — synthesize then subsume; expression-directed
/// checking rules (e.g. a lambda against a function type) arrive with later
/// constructs.
#[must_use]
fn check(expr: &CoreExpr, expected: &Ty, env: &TyEnv) -> Option<TypeError> {
    let got = synth(expr, env);
    if consistent(&got, expected) {
        None
    } else {
        Some(TypeError {
            message: format!("type mismatch: expected `{expected:?}`, found `{got:?}`"),
            span: expr.span,
        })
    }
}

/// Whether two types are *consistent* (gradual `~`): `Dyn` matches anything in
/// either direction; otherwise types must be equal. Structural equality (derived
/// on [`Ty`]) covers `Named`/`Tuple`/`Func` for free. Contract subtyping extends
/// this with a subtype arm in a later stage.
#[must_use]
fn consistent(a: &Ty, b: &Ty) -> bool {
    matches!(a, Ty::Dyn) || matches!(b, Ty::Dyn) || a == b
}

/// Convert a surface type annotation into a [`Ty`], canonicalising the primitive
/// names. Type names the checker doesn't track yet — user types, function/tuple
/// annotations — become `Dyn` (gradual, hence unchecked) until a later stage
/// teaches the checker to resolve them.
#[must_use]
fn ty_of_annotation(ann: &Type) -> Ty {
    match ann {
        Type::Name { name, .. } => match name.as_str() {
            "Int" => Ty::Int,
            "Float" => Ty::Float,
            "Bool" => Ty::Bool,
            "Str" => Ty::Str,
            _ => Ty::Dyn,
        },
        _ => Ty::Dyn,
    }
}

/// Type-check a lowered program, collecting every type error. Each function's
/// body is checked against its declared return type (`Dyn` — hence unchecked —
/// when the return is unannotated).
#[must_use]
pub fn check_program(items: &[CoreItem]) -> Vec<TypeError> {
    let mut errors = Vec::new();
    for item in items {
        if let CoreItem::Func { params, ret, body, .. } = item {
            // Bind each parameter to its declared type (unannotated → `Dyn`).
            let env: TyEnv = params
                .iter()
                .map(|p| (p.name.clone(), p.ty.as_ref().map_or(Ty::Dyn, ty_of_annotation)))
                .collect();
            let expected = ret.as_ref().map_or(Ty::Dyn, ty_of_annotation);
            if let Some(error) = check(body, &expected, &env) {
                errors.push(error);
            }
        }
    }
    errors
}

#[cfg(test)]
mod tests {
    use super::{synth, Ty, TyEnv};

    /// Lower a literal source expression to a `CoreExpr` for synthesis.
    fn core(src: &str) -> crate::core_ir::CoreExpr {
        crate::lower::lower_expr_to_core(&crate::parser::parse(src).expect("parses"))
    }

    /// Synthesize the type of a source expression in an empty environment.
    fn ty(src: &str) -> Ty {
        synth(&core(src), &TyEnv::new())
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
}
