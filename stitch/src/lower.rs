//! AST lowering pass: surface AST → core AST.
//!
//! The parser emits a faithful surface AST; this pass is the single home for
//! all desugaring. Current desugars:
//!
//!   - `SubjectlessMatch { arms, default }` → nested `Expr::If` chains

#[allow(clippy::wildcard_imports, reason = "alloc prelude for no_std")]
use crate::prelude::*;

use crate::ast::{Expr, Item, MatchArm, Method, Stmt, StrSegment};

/// Lower a full program (all top-level items) in place.
pub fn lower_program(items: &mut [Item]) {
    for item in items.iter_mut() {
        lower_item(item);
    }
}

fn lower_item(item: &mut Item) {
    match item {
        Item::Func { body, .. } => lower_expr(body),
        Item::Const { value, .. } => lower_expr(value),
        Item::On { methods, .. } | Item::Contract { methods, .. } => {
            for m in methods.iter_mut() {
                lower_method(m);
            }
        }
        Item::Prod { .. } | Item::Sum { .. } | Item::Use { .. } => {}
    }
}

fn lower_method(method: &mut Method) {
    if let Some(body) = &mut method.body {
        lower_expr(body);
    }
}

fn lower_expr(expr: &mut Expr) {
    match expr {
        Expr::SubjectlessMatch { arms, default } => {
            // Lower children first, then replace the node.
            for (cond, body) in arms.iter_mut() {
                lower_expr(cond);
                lower_expr(body);
            }
            lower_expr(default);
            // Fold into nested `Expr::If` chains (innermost = default).
            // We need to own `default`, so swap in a dummy and take ownership.
            let mut dummy = Expr::Tuple(Vec::new());
            core::mem::swap(&mut dummy, default);
            let mut result = dummy;
            for (cond, body) in arms.drain(..).rev() {
                result = Expr::If {
                    cond: Box::new(cond),
                    then: Box::new(body),
                    els: Box::new(result),
                };
            }
            *expr = result;
        }
        Expr::Binary { left, right, .. } => {
            lower_expr(left);
            lower_expr(right);
        }
        Expr::Unary { operand, .. } | Expr::Try(operand) | Expr::Spread(operand) => {
            lower_expr(operand);
        }
        Expr::Call { callee, args } => {
            lower_expr(callee);
            for arg in args.iter_mut() {
                lower_expr(&mut arg.value);
            }
        }
        Expr::Field { object, .. } | Expr::SafeField { object, .. } => lower_expr(object),
        Expr::Index { object, index } => {
            lower_expr(object);
            lower_expr(index);
        }
        Expr::Range { start, end, .. } => {
            if let Some(e) = start {
                lower_expr(e);
            }
            if let Some(e) = end {
                lower_expr(e);
            }
        }
        Expr::If { cond, then, els } => {
            lower_expr(cond);
            lower_expr(then);
            lower_expr(els);
        }
        Expr::Lambda { body, .. } => lower_expr(body),
        Expr::Tuple(elems) | Expr::List(elems) => {
            for e in elems.iter_mut() {
                lower_expr(e);
            }
        }
        Expr::Map(entries) => {
            for (k, v) in entries.iter_mut() {
                lower_expr(k);
                lower_expr(v);
            }
        }
        Expr::Str(segments) => {
            for seg in segments.iter_mut() {
                if let StrSegment::Interp(e) = seg {
                    lower_expr(e);
                }
            }
        }
        Expr::Block { stmts, result } => {
            for stmt in stmts.iter_mut() {
                lower_stmt(stmt);
            }
            if let Some(e) = result {
                lower_expr(e);
            }
        }
        Expr::Match { subject, arms } => {
            lower_expr(subject);
            for arm in arms.iter_mut() {
                lower_match_arm(arm);
            }
        }
        Expr::Int(_)
        | Expr::Float(_)
        | Expr::Bool(_)
        | Expr::Var(_)
        | Expr::SelfRef
        | Expr::Placeholder(_) => {}
    }
}

fn lower_stmt(stmt: &mut Stmt) {
    match stmt {
        Stmt::Let { value, .. } | Stmt::Assign { value, .. } => lower_expr(value),
        Stmt::Use { call, .. } => lower_expr(call),
        Stmt::Expr(e) => lower_expr(e),
    }
}

fn lower_match_arm(arm: &mut MatchArm) {
    if let Some(guard) = &mut arm.guard {
        lower_expr(guard);
    }
    lower_expr(&mut arm.body);
}
