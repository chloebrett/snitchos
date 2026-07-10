//! AST lowering pass: surface AST → core AST.
//!
//! The parser emits a faithful surface AST; this pass is the single home for
//! all desugaring. Current desugars:
//!
//!   - `SubjectlessMatch { arms, default }` → nested `ExprKind::If` chains
//!   - `Stmt::Use { binding, call }` → `call(..args, binding -> { rest })`

#[allow(clippy::wildcard_imports, reason = "alloc prelude for no_std")]
use crate::prelude::*;

use alloc::collections::BTreeSet;

use crate::ast::{Arg, BinOp, Expr, ExprKind, Item, MatchArm, Method, Pattern, Stmt, StrSegment};

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
    match &mut expr.kind {
        ExprKind::SubjectlessMatch { arms, default } => {
            // Lower children first, then replace the node.
            for (cond, body) in arms.iter_mut() {
                lower_expr(cond);
                lower_expr(body);
            }
            lower_expr(default);
            // Fold into nested `ExprKind::If` chains (innermost = default).
            // We need to own `default`, so swap in a dummy and take ownership.
            let mut dummy = Expr::bare(ExprKind::Tuple(Vec::new()));
            core::mem::swap(&mut dummy, default);
            let mut result = dummy;
            for (cond, body) in arms.drain(..).rev() {
                result = Expr::bare(ExprKind::If {
                    cond: Box::new(cond),
                    then: Box::new(body),
                    els: Box::new(result),
                });
            }
            *expr = result;
        }
        ExprKind::Binary { left, right, .. } => {
            lower_expr(left);
            lower_expr(right);
        }
        ExprKind::Unary { operand, .. } | ExprKind::Try(operand) | ExprKind::Spread(operand) => {
            lower_expr(operand);
        }
        ExprKind::Call { callee, args } => {
            lower_expr(callee);
            for arg in args.iter_mut() {
                // Lower inside the arg first (inner calls consume their own
                // placeholders), then wrap this arg if it contains any.
                lower_expr(&mut arg.value);
                lower_placeholder_arg(&mut arg.value);
            }
        }
        ExprKind::Field { object, .. } | ExprKind::SafeField { object, .. } => lower_expr(object),
        ExprKind::Index { object, index } => {
            lower_expr(object);
            lower_expr(index);
        }
        ExprKind::Range { start, end, .. } => {
            if let Some(e) = start {
                lower_expr(e);
            }
            if let Some(e) = end {
                lower_expr(e);
            }
        }
        ExprKind::If { cond, then, els } => {
            lower_expr(cond);
            lower_expr(then);
            lower_expr(els);
        }
        ExprKind::Lambda { body, .. } => lower_expr(body),
        ExprKind::Tuple(elems) | ExprKind::List(elems) => {
            for e in elems.iter_mut() {
                lower_expr(e);
            }
        }
        ExprKind::Map(entries) => {
            for (k, v) in entries.iter_mut() {
                lower_expr(k);
                lower_expr(v);
            }
        }
        ExprKind::Str(segments) => {
            for seg in segments.iter_mut() {
                if let StrSegment::Interp(e) = seg {
                    lower_expr(e);
                }
            }
        }
        ExprKind::Block { stmts, result } => {
            lower_block(stmts, result);
        }
        ExprKind::Match { subject, arms } => {
            lower_expr(subject);
            for arm in arms.iter_mut() {
                lower_match_arm(arm);
            }
        }
        ExprKind::OperatorRef(op) => {
            *expr = operator_lambda(*op);
        }
        ExprKind::Int(_)
        | ExprKind::Float(_)
        | ExprKind::Bool(_)
        | ExprKind::Var(_)
        | ExprKind::SelfRef
        | ExprKind::Placeholder(_) => {}
    }
}

/// Desugar a binary operator reference to its two-parameter lambda:
/// `op` ⇒ `(lhs, rhs) -> lhs op rhs`.
fn operator_lambda(op: BinOp) -> Expr {
    Expr::bare(ExprKind::Lambda {
        params: vec!["lhs".to_string(), "rhs".to_string()],
        body: Box::new(Expr::bare(ExprKind::Binary {
            op,
            left: Box::new(Expr::bare(ExprKind::Var("lhs".to_string()))),
            right: Box::new(Expr::bare(ExprKind::Var("rhs".to_string()))),
        })),
    })
}

/// If `expr` contains any `Placeholder` nodes, rewrite them to `Var("$x")`
/// and wrap the whole expression in a `Lambda`. Stops at `Lambda` boundaries
/// (placeholders inside written-out lambdas bind to those lambdas, not this one).
fn lower_placeholder_arg(expr: &mut Expr) {
    use alloc::collections::BTreeSet;
    let mut referenced = BTreeSet::new();
    collect_placeholders(expr, &mut referenced);
    if let Some(params) = positional_params(&referenced) {
        let mut body = Expr::bare(ExprKind::Tuple(Vec::new())); // dummy
        core::mem::swap(expr, &mut body);
        *expr = Expr::bare(ExprKind::Lambda { params, body: Box::new(body) });
    }
}

/// Rewrite `Placeholder` nodes in `expr` to `Var("$x")`, collecting the
/// `$x` param names used. Stops at `Lambda` boundaries.
fn collect_placeholders(expr: &mut Expr, params: &mut alloc::collections::BTreeSet<String>) {
    match &mut expr.kind {
        ExprKind::Placeholder(name) => {
            let param = format!("${}", name.as_deref().unwrap_or("a"));
            params.insert(param.clone());
            *expr = Expr::bare(ExprKind::Var(param));
        }
        ExprKind::Binary { left, right, .. } => {
            collect_placeholders(left, params);
            collect_placeholders(right, params);
        }
        ExprKind::Unary { operand, .. } | ExprKind::Try(operand) | ExprKind::Spread(operand) => {
            collect_placeholders(operand, params);
        }
        ExprKind::Call { callee, args } => {
            collect_placeholders(callee, params);
            for arg in args.iter_mut() {
                collect_placeholders(&mut arg.value, params);
            }
        }
        ExprKind::Field { object, .. } | ExprKind::SafeField { object, .. } => {
            collect_placeholders(object, params);
        }
        ExprKind::Index { object, index } => {
            collect_placeholders(object, params);
            collect_placeholders(index, params);
        }
        ExprKind::Range { start, end, .. } => {
            if let Some(e) = start {
                collect_placeholders(e, params);
            }
            if let Some(e) = end {
                collect_placeholders(e, params);
            }
        }
        ExprKind::If { cond, then, els } => {
            collect_placeholders(cond, params);
            collect_placeholders(then, params);
            collect_placeholders(els, params);
        }
        ExprKind::Tuple(elems) | ExprKind::List(elems) => {
            for e in elems.iter_mut() {
                collect_placeholders(e, params);
            }
        }
        ExprKind::Map(entries) => {
            for (k, v) in entries.iter_mut() {
                collect_placeholders(k, params);
                collect_placeholders(v, params);
            }
        }
        // Lambda: stop here — its body's placeholders belong to it.
        // Atoms and surface-only nodes (already lowered by the time we're called,
        // or never contain sub-expressions with placeholders).
        ExprKind::Int(_)
        | ExprKind::Float(_)
        | ExprKind::Bool(_)
        | ExprKind::Var(_)
        | ExprKind::SelfRef
        | ExprKind::Str(_)
        | ExprKind::OperatorRef(_)
        | ExprKind::SubjectlessMatch { .. }
        | ExprKind::Lambda { .. }
        | ExprKind::Block { .. }
        | ExprKind::Match { .. } => {}
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
    let mut callback_body = Expr::bare(ExprKind::Block {
        stmts: rest_stmts,
        result: rest_result,
    });
    lower_expr(&mut callback_body);

    let params: Vec<String> = binding.into_iter().collect();
    let callback = Expr::bare(ExprKind::Lambda {
        params,
        body: Box::new(callback_body),
    });
    let callback_arg = Arg { label: None, value: callback };

    // Append callback to the call or wrap in a new call.
    lower_expr(&mut call);
    let call_span = call.span;
    let desugared = match call.kind {
        ExprKind::Call { callee, mut args } => {
            args.push(callback_arg);
            ExprKind::Call { callee, args }
        }
        other => ExprKind::Call {
            callee: Box::new(Expr::new(other, call_span)),
            args: vec![callback_arg],
        },
    };

    // The use site becomes the block's result expression.
    *result = Some(Box::new(Expr::new(desugared, call_span)));
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

/// The names bound by a pattern — names that are in scope in the arm body.
pub fn pattern_bindings(pat: &Pattern) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    collect_pattern_bindings(pat, &mut names);
    names
}

fn collect_pattern_bindings(pat: &Pattern, out: &mut BTreeSet<String>) {
    match pat {
        Pattern::Binding(name) => {
            out.insert(name.clone());
        }
        Pattern::Constructor { args, .. } => {
            for arg in args {
                collect_pattern_bindings(arg, out);
            }
        }
        Pattern::Tuple(pats) | Pattern::Or(pats) => {
            for p in pats {
                collect_pattern_bindings(p, out);
            }
        }
        Pattern::Wildcard
        | Pattern::Int(_)
        | Pattern::Float(_)
        | Pattern::Bool(_)
        | Pattern::Str(_) => {}
    }
}

/// The free variables in `expr` — names referenced but not bound within the
/// expression itself, given the set of names already bound in the outer scope.
///
/// Used at closure creation to determine which local bindings to capture as
/// upvalues. Note: globals and parameters are **not** included in `bound`
/// when calling this for a lambda body; the result therefore includes
/// references to globals too, which is fine — the caller filters by whether
/// the name exists as a *local* in the defining env (via `lookup_local_cell`).
pub fn free_vars(expr: &Expr, bound: &BTreeSet<String>) -> BTreeSet<String> {
    let mut free = BTreeSet::new();
    collect_free_vars(expr, bound, &mut free);
    free
}

fn collect_free_vars(expr: &Expr, bound: &BTreeSet<String>, free: &mut BTreeSet<String>) {
    match &expr.kind {
        ExprKind::Var(name) => {
            if !bound.contains(name.as_str()) {
                free.insert(name.clone());
            }
        }
        ExprKind::Lambda { params, body } => {
            let mut inner = bound.clone();
            inner.extend(params.iter().cloned());
            collect_free_vars(body, &inner, free);
        }
        ExprKind::Block { stmts, result } => {
            let mut inner = bound.clone();
            for stmt in stmts {
                match stmt {
                    Stmt::Let { name, value, .. } => {
                        collect_free_vars(value, &inner, free);
                        inner.insert(name.clone());
                    }
                    Stmt::Assign { target, value } => {
                        collect_free_vars(target, &inner, free);
                        collect_free_vars(value, &inner, free);
                    }
                    Stmt::Use { call, binding } => {
                        collect_free_vars(call, &inner, free);
                        if let Some(name) = binding {
                            inner.insert(name.clone());
                        }
                    }
                    Stmt::Expr(e) => collect_free_vars(e, &inner, free),
                }
            }
            if let Some(e) = result {
                collect_free_vars(e, &inner, free);
            }
        }
        ExprKind::Match { subject, arms } => {
            collect_free_vars(subject, bound, free);
            for arm in arms {
                let mut arm_bound = bound.clone();
                arm_bound.extend(pattern_bindings(&arm.pattern));
                if let Some(guard) = &arm.guard {
                    collect_free_vars(guard, &arm_bound, free);
                }
                collect_free_vars(&arm.body, &arm_bound, free);
            }
        }
        ExprKind::Binary { left, right, .. } => {
            collect_free_vars(left, bound, free);
            collect_free_vars(right, bound, free);
        }
        ExprKind::Unary { operand, .. } | ExprKind::Try(operand) | ExprKind::Spread(operand) => {
            collect_free_vars(operand, bound, free);
        }
        ExprKind::Call { callee, args } => {
            collect_free_vars(callee, bound, free);
            for arg in args {
                collect_free_vars(&arg.value, bound, free);
            }
        }
        ExprKind::Field { object, .. } | ExprKind::SafeField { object, .. } => {
            collect_free_vars(object, bound, free);
        }
        ExprKind::Index { object, index } => {
            collect_free_vars(object, bound, free);
            collect_free_vars(index, bound, free);
        }
        ExprKind::Range { start, end, .. } => {
            if let Some(e) = start {
                collect_free_vars(e, bound, free);
            }
            if let Some(e) = end {
                collect_free_vars(e, bound, free);
            }
        }
        ExprKind::If { cond, then, els } => {
            collect_free_vars(cond, bound, free);
            collect_free_vars(then, bound, free);
            collect_free_vars(els, bound, free);
        }
        ExprKind::Tuple(elems) | ExprKind::List(elems) => {
            for e in elems {
                collect_free_vars(e, bound, free);
            }
        }
        ExprKind::Map(entries) => {
            for (k, v) in entries {
                collect_free_vars(k, bound, free);
                collect_free_vars(v, bound, free);
            }
        }
        ExprKind::Str(segments) => {
            for seg in segments {
                if let StrSegment::Interp(e) = seg {
                    collect_free_vars(e, bound, free);
                }
            }
        }
        ExprKind::Int(_)
        | ExprKind::Float(_)
        | ExprKind::Bool(_)
        | ExprKind::SelfRef
        | ExprKind::OperatorRef(_)
        | ExprKind::SubjectlessMatch { .. }
        | ExprKind::Placeholder(_) => {}
    }
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
        match &expr.kind {
            ExprKind::Block { stmts, result } => {
                stmts.iter().any(|s| matches!(s, Stmt::Use { .. }) || stmt_has_use(s))
                    || result.as_deref().map_or(false, expr_has_use)
            }
            ExprKind::Call { callee, args } => {
                expr_has_use(callee) || args.iter().any(|a| expr_has_use(&a.value))
            }
            ExprKind::Lambda { body, .. } => expr_has_use(body),
            ExprKind::If { cond, then, els } => {
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
