//! AST lowering pass: surface AST → core AST.
//!
//! The parser emits a faithful surface AST; this pass is the single home for
//! all desugaring. Current desugars:
//!
//!   - `SubjectlessMatch { arms, default }` → nested `Expr::If` chains
//!   - `Stmt::Use { binding, call }` → `call(..args, binding -> { rest })`

#[allow(clippy::wildcard_imports, reason = "alloc prelude for no_std")]
use crate::prelude::*;

use crate::ast::{Arg, BinOp, Expr, Item, MatchArm, Method, Stmt, StrSegment};

/// Lower a single expression in place (e.g. for a REPL line or a test `run`).
pub fn lower(expr: &mut Expr) {
    lower_expr(expr);
}

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
                // Lower inside the arg first (inner calls consume their own
                // placeholders), then wrap this arg if it contains any.
                lower_expr(&mut arg.value);
                lower_placeholder_arg(&mut arg.value);
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
            lower_block(stmts, result);
        }
        Expr::Match { subject, arms } => {
            lower_expr(subject);
            for arm in arms.iter_mut() {
                lower_match_arm(arm);
            }
        }
        Expr::OperatorRef(op) => {
            *expr = operator_lambda(*op);
        }
        Expr::Int(_)
        | Expr::Float(_)
        | Expr::Bool(_)
        | Expr::Var(_)
        | Expr::SelfRef
        | Expr::Placeholder(_) => {}
    }
}

/// Desugar a binary operator reference to its two-parameter lambda:
/// `op` ⇒ `(lhs, rhs) -> lhs op rhs`.
fn operator_lambda(op: BinOp) -> Expr {
    Expr::Lambda {
        params: vec!["lhs".to_string(), "rhs".to_string()],
        body: Box::new(Expr::Binary {
            op,
            left: Box::new(Expr::Var("lhs".to_string())),
            right: Box::new(Expr::Var("rhs".to_string())),
        }),
    }
}

/// If `expr` contains any `Placeholder` nodes, rewrite them to `Var("$x")`
/// and wrap the whole expression in a `Lambda`. Stops at `Lambda` boundaries
/// (placeholders inside written-out lambdas bind to those lambdas, not this one).
fn lower_placeholder_arg(expr: &mut Expr) {
    use alloc::collections::BTreeSet;
    let mut referenced = BTreeSet::new();
    collect_placeholders(expr, &mut referenced);
    if let Some(params) = positional_params(&referenced) {
        let mut body = Expr::Tuple(Vec::new()); // dummy
        core::mem::swap(expr, &mut body);
        *expr = Expr::Lambda { params, body: Box::new(body) };
    }
}

/// Rewrite `Placeholder` nodes in `expr` to `Var("$x")`, collecting the
/// `$x` param names used. Stops at `Lambda` boundaries.
fn collect_placeholders(expr: &mut Expr, params: &mut alloc::collections::BTreeSet<String>) {
    match expr {
        Expr::Placeholder(name) => {
            let param = format!("${}", name.as_deref().unwrap_or("a"));
            params.insert(param.clone());
            *expr = Expr::Var(param);
        }
        Expr::Binary { left, right, .. } => {
            collect_placeholders(left, params);
            collect_placeholders(right, params);
        }
        Expr::Unary { operand, .. } | Expr::Try(operand) | Expr::Spread(operand) => {
            collect_placeholders(operand, params);
        }
        Expr::Call { callee, args } => {
            collect_placeholders(callee, params);
            for arg in args.iter_mut() {
                collect_placeholders(&mut arg.value, params);
            }
        }
        Expr::Field { object, .. } | Expr::SafeField { object, .. } => {
            collect_placeholders(object, params);
        }
        Expr::Index { object, index } => {
            collect_placeholders(object, params);
            collect_placeholders(index, params);
        }
        Expr::Range { start, end, .. } => {
            if let Some(e) = start {
                collect_placeholders(e, params);
            }
            if let Some(e) = end {
                collect_placeholders(e, params);
            }
        }
        Expr::If { cond, then, els } => {
            collect_placeholders(cond, params);
            collect_placeholders(then, params);
            collect_placeholders(els, params);
        }
        Expr::Tuple(elems) | Expr::List(elems) => {
            for e in elems.iter_mut() {
                collect_placeholders(e, params);
            }
        }
        Expr::Map(entries) => {
            for (k, v) in entries.iter_mut() {
                collect_placeholders(k, params);
                collect_placeholders(v, params);
            }
        }
        // Lambda: stop here — its body's placeholders belong to it.
        // Atoms and surface-only nodes (already lowered by the time we're called,
        // or never contain sub-expressions with placeholders).
        Expr::Int(_)
        | Expr::Float(_)
        | Expr::Bool(_)
        | Expr::Var(_)
        | Expr::SelfRef
        | Expr::Str(_)
        | Expr::OperatorRef(_)
        | Expr::SubjectlessMatch { .. }
        | Expr::Lambda { .. }
        | Expr::Block { .. }
        | Expr::Match { .. } => {}
    }
}

/// Turn a set of referenced placeholder names into a positional param list.
/// The letter is the index (`$a`=0, `$b`=1, …); unreferenced lower slots
/// become `_` holes (`$b` alone ⇒ `(_, $b)`). `None` when empty.
fn positional_params(referenced: &alloc::collections::BTreeSet<String>) -> Option<Vec<String>> {
    let max = referenced
        .iter()
        .filter_map(|name| name.strip_prefix('$').and_then(|s| s.chars().next()))
        .map(|letter| (letter as usize) - ('a' as usize))
        .max()?;
    let params = (0..=max)
        .map(|index| {
            let letter = (b'a' + index as u8) as char;
            let name = format!("${letter}");
            if referenced.contains(&name) { name } else { "_".to_string() }
        })
        .collect();
    Some(params)
}

/// Lower a block (stmts + optional result) in place.
///
/// Scans for the first `Stmt::Use` and transforms it:
///
///   `use x <- f(a); rest` → `f(a, x -> { rest })`
///
/// The callback (rest-of-block) is itself lowered recursively, so nested
/// `use <-` statements are handled naturally.
fn lower_block(stmts: &mut Vec<Stmt>, result: &mut Option<Box<Expr>>) {
    // Find the first Stmt::Use.
    let use_idx = stmts.iter().position(|s| matches!(s, Stmt::Use { .. }));
    let Some(idx) = use_idx else {
        // No use <-, just recursively lower stmts and result.
        for stmt in stmts.iter_mut() {
            lower_stmt(stmt);
        }
        if let Some(e) = result {
            lower_expr(e);
        }
        return;
    };

    // Lower everything before the `use` statement.
    for stmt in &mut stmts[..idx] {
        lower_stmt(stmt);
    }

    // Pull out the `use` statement and everything after it.
    let use_stmt = stmts.remove(idx);
    let rest_stmts: Vec<Stmt> = stmts.drain(idx..).collect();
    let rest_result: Option<Box<Expr>> = result.take();

    let Stmt::Use { binding, mut call } = use_stmt else {
        unreachable!()
    };

    // Build the callback lambda: `binding -> { rest }`.
    // Lower the rest block recursively first (handles nested use <-).
    let mut callback_body = Expr::Block {
        stmts: rest_stmts,
        result: rest_result,
    };
    lower_expr(&mut callback_body);

    let params: Vec<String> = binding.into_iter().collect();
    let callback = Expr::Lambda {
        params,
        body: Box::new(callback_body),
    };
    let callback_arg = Arg { label: None, value: callback };

    // Append callback to the call or wrap in a new call.
    lower_expr(&mut call);
    let desugared = if let Expr::Call { callee, mut args } = call {
        args.push(callback_arg);
        Expr::Call { callee, args }
    } else {
        Expr::Call {
            callee: Box::new(call),
            args: vec![callback_arg],
        }
    };

    // The use site becomes the block's result expression.
    *result = Some(Box::new(desugared));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse_program;

    fn has_use_stmt(items: &[Item]) -> bool {
        items.iter().any(|item| match item {
            Item::Func { body, .. } => expr_has_use(body),
            Item::Const { value, .. } => expr_has_use(value),
            _ => false,
        })
    }

    fn expr_has_use(expr: &Expr) -> bool {
        match expr {
            Expr::Block { stmts, result } => {
                stmts.iter().any(|s| matches!(s, Stmt::Use { .. }) || stmt_has_use(s))
                    || result.as_deref().map_or(false, expr_has_use)
            }
            Expr::Call { callee, args } => {
                expr_has_use(callee) || args.iter().any(|a| expr_has_use(&a.value))
            }
            Expr::Lambda { body, .. } => expr_has_use(body),
            Expr::If { cond, then, els } => {
                expr_has_use(cond) || expr_has_use(then) || expr_has_use(els)
            }
            _ => false,
        }
    }

    fn stmt_has_use(stmt: &Stmt) -> bool {
        match stmt {
            Stmt::Use { .. } => true,
            Stmt::Let { value, .. } | Stmt::Assign { value, .. } => expr_has_use(value),
            Stmt::Expr(e) => expr_has_use(e),
        }
    }

    #[test]
    fn use_desugar_removed_by_lowering() {
        // After lowering, Stmt::Use must be gone — replaced by a call + lambda.
        let mut items = parse_program("main() = { use x <- f(1)  x + 1 }").unwrap();
        assert!(has_use_stmt(&items), "Stmt::Use should be present before lowering");
        lower_program(&mut items);
        assert!(!has_use_stmt(&items), "Stmt::Use should be gone after lowering");
    }
}
