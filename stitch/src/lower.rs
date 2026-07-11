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
use crate::core_ir::{
    CoreArg, CoreExpr, CoreExprKind, CoreItem, CoreMatchArm, CoreMethod, CoreStmt, CoreStrSegment,
};

/// Lower a single expression in place (e.g. for a REPL line or a test `run`).
pub fn lower(expr: &mut Expr) {
    lower_expr(expr);
}

/// Lower a surface expression into a core expression, desugaring the surface-only
/// nodes (`SubjectlessMatch`, `OperatorRef`, `Placeholder`, `use <-`) into ordinary
/// core nodes. Every core node carries the span of the surface node it came from.
///
/// Implemented as the existing surface→surface desugar (`lower_expr`) followed by a
/// pure 1:1 reshape into the core IR (`to_core`). After `lower_expr` no surface-only
/// node remains, so `to_core` is a total structural map.
#[must_use]
pub fn lower_expr_to_core(expr: &Expr) -> CoreExpr {
    let mut desugared = expr.clone();
    lower_expr(&mut desugared);
    to_core(&desugared)
}

/// Reshape a fully-desugared surface expression into the core IR. Panics if a
/// surface-only node survives (a lowering bug — `lower_expr` must have removed it).
fn to_core(expr: &Expr) -> CoreExpr {
    let span = expr.span;
    let kind = match &expr.kind {
        ExprKind::Int(n) => CoreExprKind::Int(*n),
        ExprKind::Float(f) => CoreExprKind::Float(*f),
        ExprKind::Bool(b) => CoreExprKind::Bool(*b),
        ExprKind::Var(name) => CoreExprKind::Var(name.clone()),
        ExprKind::SelfRef => CoreExprKind::SelfRef,
        ExprKind::Spread(base) => CoreExprKind::Spread(Box::new(to_core(base))),
        ExprKind::Binary { op, left, right } => CoreExprKind::Binary {
            op: *op,
            left: Box::new(to_core(left)),
            right: Box::new(to_core(right)),
        },
        ExprKind::Unary { op, operand } => CoreExprKind::Unary {
            op: *op,
            operand: Box::new(to_core(operand)),
        },
        ExprKind::Call { callee, args } => CoreExprKind::Call {
            callee: Box::new(to_core(callee)),
            args: args.iter().map(to_core_arg).collect(),
        },
        ExprKind::Field { object, name } => CoreExprKind::Field {
            object: Box::new(to_core(object)),
            name: name.clone(),
        },
        ExprKind::SafeField { object, name } => CoreExprKind::SafeField {
            object: Box::new(to_core(object)),
            name: name.clone(),
        },
        ExprKind::Try(operand) => CoreExprKind::Try(Box::new(to_core(operand))),
        ExprKind::Index { object, index } => CoreExprKind::Index {
            object: Box::new(to_core(object)),
            index: Box::new(to_core(index)),
        },
        ExprKind::Lambda { params, body } => CoreExprKind::Lambda {
            params: params.clone(),
            body: Rc::new(to_core(body)),
        },
        ExprKind::Range { start, end, inclusive } => CoreExprKind::Range {
            start: start.as_ref().map(|e| Box::new(to_core(e))),
            end: end.as_ref().map(|e| Box::new(to_core(e))),
            inclusive: *inclusive,
        },
        ExprKind::If { cond, then, els } => CoreExprKind::If {
            cond: Box::new(to_core(cond)),
            then: Box::new(to_core(then)),
            els: Box::new(to_core(els)),
        },
        ExprKind::Tuple(elems) => CoreExprKind::Tuple(elems.iter().map(to_core).collect()),
        ExprKind::List(elems) => CoreExprKind::List(elems.iter().map(to_core).collect()),
        ExprKind::Map(entries) => {
            CoreExprKind::Map(entries.iter().map(|(k, v)| (to_core(k), to_core(v))).collect())
        }
        ExprKind::Str(segments) => CoreExprKind::Str(segments.iter().map(to_core_segment).collect()),
        ExprKind::Block { stmts, result } => CoreExprKind::Block {
            stmts: stmts.iter().map(to_core_stmt).collect(),
            result: result.as_ref().map(|e| Box::new(to_core(e))),
        },
        ExprKind::Match { subject, arms } => CoreExprKind::Match {
            subject: Box::new(to_core(subject)),
            arms: arms.iter().map(to_core_arm).collect(),
        },
        ExprKind::Handle { op, handler, body } => CoreExprKind::Handle {
            op: op.clone(),
            handler: Box::new(to_core(handler)),
            body: Box::new(to_core(body)),
        },
        ExprKind::Without { cap, body } => CoreExprKind::Without {
            cap: cap.clone(),
            body: Box::new(to_core(body)),
        },
        ExprKind::Placeholder(_) | ExprKind::OperatorRef(_) | ExprKind::SubjectlessMatch { .. } => {
            unreachable!("surface-only node survived lowering: {:?}", expr.kind)
        }
    };
    CoreExpr::new(kind, span)
}

fn to_core_arg(arg: &Arg) -> CoreArg {
    CoreArg { label: arg.label.clone(), value: to_core(&arg.value) }
}

fn to_core_segment(segment: &StrSegment) -> CoreStrSegment {
    match segment {
        StrSegment::Lit(text) => CoreStrSegment::Lit(text.clone()),
        StrSegment::Interp(expr) => CoreStrSegment::Interp(Box::new(to_core(expr))),
    }
}

fn to_core_stmt(stmt: &Stmt) -> CoreStmt {
    match stmt {
        Stmt::Let { name, mutable, value } => CoreStmt::Let {
            name: name.clone(),
            mutable: *mutable,
            value: to_core(value),
        },
        Stmt::Assign { target, value } => CoreStmt::Assign {
            target: to_core(target),
            value: to_core(value),
        },
        Stmt::Expr(expr) => CoreStmt::Expr(to_core(expr)),
        Stmt::Use { .. } => {
            unreachable!("Stmt::Use survived lowering — lower_block should have desugared it")
        }
    }
}

fn to_core_arm(arm: &MatchArm) -> CoreMatchArm {
    CoreMatchArm {
        pattern: arm.pattern.clone(),
        guard: arm.guard.as_ref().map(to_core),
        body: to_core(&arm.body),
    }
}

/// Lower a whole program's top-level items into the core IR.
#[must_use]
pub fn lower_items_to_core(items: &[Item]) -> Vec<CoreItem> {
    items.iter().map(lower_item_to_core).collect()
}

/// Lower one top-level item, lowering its executable bodies (`Func.body`,
/// `Const.value`, method bodies) to `CoreExpr`. Type metadata passes through.
#[must_use]
pub fn lower_item_to_core(item: &Item) -> CoreItem {
    match item {
        Item::Prod { name, generics, fields, public } => CoreItem::Prod {
            name: name.clone(),
            generics: generics.clone(),
            fields: fields.clone(),
            public: *public,
        },
        Item::Sum { name, generics, variants, public } => CoreItem::Sum {
            name: name.clone(),
            generics: generics.clone(),
            variants: variants.clone(),
            public: *public,
        },
        Item::Func { name, params, ret, uses, body, public } => CoreItem::Func {
            name: name.clone(),
            params: params.clone(),
            ret: ret.clone(),
            uses: uses.iter().map(|effect| effect.name.clone()).collect(),
            body: Rc::new(lower_expr_to_core(body)),
            public: *public,
        },
        Item::Contract { name, generics, methods } => CoreItem::Contract {
            name: name.clone(),
            generics: generics.clone(),
            methods: methods.iter().map(to_core_method).collect(),
        },
        Item::On { target, contract, methods } => CoreItem::On {
            target: target.clone(),
            contract: contract.clone(),
            methods: methods.iter().map(to_core_method).collect(),
        },
        Item::Const { name, mutable, value, public } => CoreItem::Const {
            name: name.clone(),
            mutable: *mutable,
            value: lower_expr_to_core(value),
            public: *public,
        },
        Item::Use { module, names } => CoreItem::Use {
            module: module.clone(),
            names: names.clone(),
        },
    }
}

fn to_core_method(method: &Method) -> CoreMethod {
    CoreMethod {
        name: method.name.clone(),
        modifier: method.modifier.clone(),
        params: method.params.clone(),
        ret: method.ret.clone(),
        uses: method.uses.iter().map(|effect| effect.name.clone()).collect(),
        body: method.body.as_ref().map(lower_expr_to_core),
    }
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
        ExprKind::Handle { op: _, handler, body } => {
            lower_expr(handler);
            lower_expr(body);
        }
        ExprKind::Without { cap: _, body } => {
            lower_expr(body);
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
        | ExprKind::Match { .. }
        | ExprKind::Handle { .. }
        | ExprKind::Without { .. } => {}
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
        ExprKind::Handle { op: _, handler, body } => {
            collect_free_vars(handler, bound, free);
            collect_free_vars(body, bound, free);
        }
        ExprKind::Without { cap: _, body } => {
            collect_free_vars(body, bound, free);
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

/// The free variables in a *core* expression — the analog of [`free_vars`] over the
/// desugared core IR. Used by `eval_core`'s `Lambda` arm to capture upvalues. The
/// core IR has no surface-only nodes, so this is a strict subset of the surface
/// walk (no `Placeholder`/`OperatorRef`/`SubjectlessMatch`, and `CoreStmt` has no
/// `Use`).
#[must_use]
pub fn free_vars_core(expr: &CoreExpr, bound: &BTreeSet<String>) -> BTreeSet<String> {
    let mut free = BTreeSet::new();
    collect_free_vars_core(expr, bound, &mut free);
    free
}

fn collect_free_vars_core(expr: &CoreExpr, bound: &BTreeSet<String>, free: &mut BTreeSet<String>) {
    match &expr.kind {
        CoreExprKind::Var(name) => {
            if !bound.contains(name.as_str()) {
                free.insert(name.clone());
            }
        }
        CoreExprKind::Lambda { params, body } => {
            let mut inner = bound.clone();
            inner.extend(params.iter().cloned());
            collect_free_vars_core(body, &inner, free);
        }
        CoreExprKind::Block { stmts, result } => {
            let mut inner = bound.clone();
            for stmt in stmts {
                match stmt {
                    CoreStmt::Let { name, value, .. } => {
                        collect_free_vars_core(value, &inner, free);
                        inner.insert(name.clone());
                    }
                    CoreStmt::Assign { target, value } => {
                        collect_free_vars_core(target, &inner, free);
                        collect_free_vars_core(value, &inner, free);
                    }
                    CoreStmt::Expr(e) => collect_free_vars_core(e, &inner, free),
                }
            }
            if let Some(e) = result {
                collect_free_vars_core(e, &inner, free);
            }
        }
        CoreExprKind::Match { subject, arms } => {
            collect_free_vars_core(subject, bound, free);
            for arm in arms {
                let mut arm_bound = bound.clone();
                arm_bound.extend(pattern_bindings(&arm.pattern));
                if let Some(guard) = &arm.guard {
                    collect_free_vars_core(guard, &arm_bound, free);
                }
                collect_free_vars_core(&arm.body, &arm_bound, free);
            }
        }
        CoreExprKind::Handle { op: _, handler, body } => {
            collect_free_vars_core(handler, bound, free);
            collect_free_vars_core(body, bound, free);
        }
        CoreExprKind::Without { cap: _, body } => {
            collect_free_vars_core(body, bound, free);
        }
        CoreExprKind::Binary { left, right, .. } => {
            collect_free_vars_core(left, bound, free);
            collect_free_vars_core(right, bound, free);
        }
        CoreExprKind::Unary { operand, .. }
        | CoreExprKind::Try(operand)
        | CoreExprKind::Spread(operand) => {
            collect_free_vars_core(operand, bound, free);
        }
        CoreExprKind::Call { callee, args } => {
            collect_free_vars_core(callee, bound, free);
            for arg in args {
                collect_free_vars_core(&arg.value, bound, free);
            }
        }
        CoreExprKind::Field { object, .. } | CoreExprKind::SafeField { object, .. } => {
            collect_free_vars_core(object, bound, free);
        }
        CoreExprKind::Index { object, index } => {
            collect_free_vars_core(object, bound, free);
            collect_free_vars_core(index, bound, free);
        }
        CoreExprKind::Range { start, end, .. } => {
            if let Some(e) = start {
                collect_free_vars_core(e, bound, free);
            }
            if let Some(e) = end {
                collect_free_vars_core(e, bound, free);
            }
        }
        CoreExprKind::If { cond, then, els } => {
            collect_free_vars_core(cond, bound, free);
            collect_free_vars_core(then, bound, free);
            collect_free_vars_core(els, bound, free);
        }
        CoreExprKind::Tuple(elems) | CoreExprKind::List(elems) => {
            for e in elems {
                collect_free_vars_core(e, bound, free);
            }
        }
        CoreExprKind::Map(entries) => {
            for (k, v) in entries {
                collect_free_vars_core(k, bound, free);
                collect_free_vars_core(v, bound, free);
            }
        }
        CoreExprKind::Str(segments) => {
            for seg in segments {
                if let CoreStrSegment::Interp(e) = seg {
                    collect_free_vars_core(e, bound, free);
                }
            }
        }
        CoreExprKind::Int(_)
        | CoreExprKind::Float(_)
        | CoreExprKind::Bool(_)
        | CoreExprKind::SelfRef => {}
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

    /// Parse a single expression and lower it to core.
    fn lc(src: &str) -> CoreExpr {
        let expr = crate::parser::parse(src).expect("expr should parse");
        lower_expr_to_core(&expr)
    }

    #[test]
    fn subjectless_match_lowers_to_nested_if() {
        use crate::core_ir::CoreExprKind;
        let core = lc(r#"match { n > 0 => "pos"  _ => "neg" }"#);
        assert!(matches!(core.kind, CoreExprKind::If { .. }), "got {:?}", core.kind);
    }

    #[test]
    fn operator_ref_lowers_to_a_two_param_lambda() {
        use crate::core_ir::CoreExprKind;
        // `f(+)` — the `+` argument lowers to `(lhs, rhs) -> lhs + rhs`.
        let core = lc("f(+)");
        let CoreExprKind::Call { args, .. } = core.kind else { panic!("expected Call") };
        let CoreExprKind::Lambda { params, .. } = &args[0].value.kind else {
            panic!("expected Lambda, got {:?}", args[0].value.kind)
        };
        assert_eq!(params.len(), 2);
    }

    #[test]
    fn use_block_lowers_to_a_call() {
        use crate::core_ir::CoreExprKind;
        // A block whose first statement is `use <-` desugars: the `use` statement is
        // gone and the block's result becomes a call whose last argument is the
        // rest-of-block callback.
        let core = lc("{ use x <- f(1)  x + 1 }");
        let CoreExprKind::Block { stmts, result } = core.kind else {
            panic!("expected Block, got {:?}", core.kind)
        };
        assert!(stmts.is_empty(), "the `use` statement should be desugared away");
        let result = result.expect("block should have a result expression");
        assert!(
            matches!(result.kind, CoreExprKind::Call { .. }),
            "use should desugar to a call, got {:?}", result.kind
        );
    }

    #[test]
    fn placeholder_lowers_to_a_lambda_argument() {
        use crate::core_ir::CoreExprKind;
        // `f($)` — the bare placeholder argument becomes a one-param lambda.
        let core = lc("f($)");
        let CoreExprKind::Call { args, .. } = core.kind else { panic!("expected Call") };
        assert!(
            matches!(args[0].value.kind, CoreExprKind::Lambda { .. }),
            "got {:?}", args[0].value.kind
        );
    }

    #[test]
    fn lowering_preserves_the_surface_span() {
        use crate::lexer::Span;
        // A lowered binary keeps the span its surface node carried.
        let expr = crate::parser::parse("a + b").expect("parse");
        let surface_span = expr.span;
        let core = lower_expr_to_core(&expr);
        assert_eq!(core.span, surface_span);
        assert_eq!(core.span, Span { start: 0, end: 5 });
    }

    #[test]
    fn a_function_item_lowers_its_body_to_core() {
        use crate::core_ir::{CoreExprKind, CoreItem};
        let items = parse_program("main() = 1 + 2").expect("parse");
        let core = lower_items_to_core(&items);
        let CoreItem::Func { name, body, .. } = &core[0] else { panic!("expected Func") };
        assert_eq!(name, "main");
        assert!(matches!(body.kind, CoreExprKind::Binary { .. }), "got {:?}", body.kind);
    }

    #[test]
    fn a_function_body_is_desugared_during_item_lowering() {
        use crate::core_ir::{CoreExprKind, CoreItem};
        // `f(+)` in a function body must desugar the operator ref to a lambda.
        let items = parse_program("g() = f(+)").expect("parse");
        let core = lower_items_to_core(&items);
        let CoreItem::Func { body, .. } = &core[0] else { panic!("expected Func") };
        let CoreExprKind::Call { args, .. } = &body.kind else { panic!("expected Call") };
        assert!(
            matches!(args[0].value.kind, CoreExprKind::Lambda { .. }),
            "operator ref should desugar to a lambda, got {:?}", args[0].value.kind
        );
    }

    #[test]
    fn a_type_declaration_passes_through_item_lowering() {
        use crate::core_ir::CoreItem;
        let items = parse_program("prod Point(x: Int, y: Int)").expect("parse");
        let core = lower_items_to_core(&items);
        let CoreItem::Prod { name, fields, .. } = &core[0] else { panic!("expected Prod") };
        assert_eq!(name, "Point");
        assert_eq!(fields.len(), 2);
    }
}
