//! Printer: AST → source. The inverse of [`crate::parser`], to within layout.
//!
//! Stitch's grammar is whitespace-insensitive, so a *generated* program (see the
//! `babble` crate) has no reason to carry newlines or indentation — and doesn't.
//! That makes generated corpus text a distribution real Stitch never occupies.
//! Rather than teach the generator layout, parse its output and print the AST:
//! the tree is already known-good, and layout becomes one module's problem.
//!
//! The contract is a round-trip — `parse(print(ast)) == ast` — not a byte
//! format. Spans move; shape does not. Comments are not in the AST (the lexer
//! skips them), so they do not survive; this is a printer for generated code,
//! not a formatter for hand-written files.
//!
//! ## Parenthesisation
//!
//! The AST records nesting; the source records operators. Printing must put back
//! exactly the parentheses the tree implies and *no others* — an
//! over-parenthesising printer is correct but emits a distribution real Stitch
//! never contains, which as training data is worse than useless.
//!
//! The rule is the parser's own precedence climb, run backwards: every position
//! carries the minimum binding power the parser would accept there, and a
//! subexpression whose own binding power falls below it gets wrapped. The
//! binding powers come from [`crate::parser`] rather than a second table here,
//! so the two cannot drift.

#[allow(clippy::wildcard_imports, reason = "alloc prelude for no_std")]
use crate::prelude::*;

use crate::ast::{
    Arg, BinOp, Effect, Expr, ExprKind, Field, Item, Method, MethodModifier, Param, Pattern, Stmt,
    StrSegment, Type, UnOp, Variant,
};
use crate::parser::{binding_power, is_non_assoc};

/// Binding power of a prefix operator's operand: `parse_prefix` recurses into
/// itself, so `-a * b` is `(-a) * b` and any binary operand needs parens.
const PREFIX_BP: u8 = 15;
/// Binding power of a postfix chain's object (`f(x)`, `p.x`, `xs[0]`, `e?`) —
/// tighter than prefix, since `-p.x` is `-(p.x)`.
const POSTFIX_BP: u8 = 17;
/// Binding power of an atom: never parenthesised on its own account.
const ATOM_BP: u8 = 19;
/// The loosest position — a lambda body, a delimited element, a statement.
/// Only here can a lambda or a `=>` conditional appear bare.
const LOOSEST_BP: u8 = 0;
/// One level above loosest: a conditional's own branches parse here, so a
/// nested lambda or conditional must be parenthesised.
const BRANCH_BP: u8 = 1;

/// One level of block indentation.
const INDENT: &str = "    ";

/// Print a whole program.
///
/// Top-level items are separated by a blank line, except runs of `use` imports,
/// which real Stitch keeps together at the head of a module.
#[must_use]
pub fn print_program(items: &[Item]) -> String {
    let mut out = String::new();
    let mut prev: Option<&Item> = None;
    for item in items {
        if let Some(prev) = prev
            && !(matches!(prev, Item::Use { .. }) && matches!(item, Item::Use { .. }))
        {
            out.push('\n');
        }
        out.push_str(&print_item(item, 0));
        out.push('\n');
        prev = Some(item);
    }
    out
}

/// Print a single expression at the loosest precedence.
#[must_use]
pub fn print_expr(expr: &Expr) -> String {
    print_at(expr, LOOSEST_BP, 0)
}

fn indent(depth: usize) -> String {
    INDENT.repeat(depth)
}

fn print_item(item: &Item, depth: usize) -> String {
    match item {
        Item::Const { name, mutable, value, public } => {
            let mutability = if *mutable { "mut " } else { "" };
            format!(
                "{}{}let {mutability}{name} = {}",
                indent(depth),
                vis(*public),
                print_body(value, depth)
            )
        }
        Item::Use { module, names } => {
            let selection = names.as_ref().map_or(String::new(), |names| {
                format!(".{{{}}}", names.join(", "))
            });
            format!("{}use {module}{selection}", indent(depth))
        }
        Item::Prod { name, generics, fields, public } => format!(
            "{}{}prod {name}{}({})",
            indent(depth),
            vis(*public),
            print_generics(generics),
            print_fields(fields)
        ),
        Item::Sum { name, generics, variants, public } => format!(
            "{}{}sum {name}{} = {}",
            indent(depth),
            vis(*public),
            print_generics(generics),
            variants.iter().map(print_variant).collect::<Vec<_>>().join(" | ")
        ),
        Item::Func { name, params, ret, uses, body, public } => format!(
            "{}{}{name}({}){}{} = {}",
            indent(depth),
            vis(*public),
            print_params(params),
            print_ret(ret.as_ref()),
            print_uses(uses),
            print_body(body, depth)
        ),
        Item::Contract { name, generics, methods } => format!(
            "{}contract {name}{} {}",
            indent(depth),
            print_generics(generics),
            print_method_block(methods, depth)
        ),
        Item::On { target, contract, methods } => {
            let conformance = contract
                .as_ref()
                .map_or(String::new(), |c| format!(" : {}", print_type(c)));
            format!(
                "{}on {}{conformance} {}",
                indent(depth),
                print_type(target),
                print_method_block(methods, depth)
            )
        }
    }
}

/// The `ext` export marker.
fn vis(public: bool) -> &'static str {
    if public { "ext " } else { "" }
}

fn print_generics(generics: &[String]) -> String {
    if generics.is_empty() {
        return String::new();
    }
    format!("<{}>", generics.join(", "))
}

fn print_ret(ret: Option<&Type>) -> String {
    ret.map_or(String::new(), |ty| format!(" -> {}", print_type(ty)))
}

/// The `uses Cap, …` effect row. Spans are metadata; only the names print.
fn print_uses(uses: &[Effect]) -> String {
    if uses.is_empty() {
        return String::new();
    }
    let names: Vec<&str> = uses.iter().map(|e| e.name.as_str()).collect();
    format!(" uses {}", names.join(", "))
}

fn print_params(params: &[Param]) -> String {
    params
        .iter()
        .map(|p| match &p.ty {
            Some(ty) => format!("{}: {}", p.name, print_type(ty)),
            None => p.name.clone(),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn print_fields(fields: &[Field]) -> String {
    fields.iter().map(print_field).collect::<Vec<_>>().join(", ")
}

fn print_field(field: &Field) -> String {
    let mutability = if field.mutable { "mut " } else { "" };
    let ty = print_type(&field.ty);
    match &field.name {
        Some(name) => format!("{}{mutability}{name}: {ty}", vis(field.public)),
        None => format!("{}{mutability}{ty}", vis(field.public)),
    }
}

fn print_variant(variant: &Variant) -> String {
    if variant.fields.is_empty() {
        return variant.name.clone();
    }
    format!("{}({})", variant.name, print_fields(&variant.fields))
}

/// A `contract`/`on` body: one method per line, or `{ }` when empty.
fn print_method_block(methods: &[Method], depth: usize) -> String {
    if methods.is_empty() {
        return String::from("{ }");
    }
    let mut out = String::from("{\n");
    for method in methods {
        out.push_str(&indent(depth + 1));
        out.push_str(&print_method(method, depth + 1));
        out.push('\n');
    }
    out.push_str(&indent(depth));
    out.push('}');
    out
}

fn print_method(method: &Method, depth: usize) -> String {
    let modifier = match method.modifier {
        MethodModifier::Instance => "",
        MethodModifier::Mut => "mut ",
        MethodModifier::Free => "free ",
    };
    let body = method
        .body
        .as_ref()
        .map_or(String::new(), |b| format!(" = {}", print_body(b, depth)));
    format!(
        "{modifier}{}({}){}{}{body}",
        method.name,
        print_params(&method.params),
        print_ret(method.ret.as_ref()),
        print_uses(&method.uses)
    )
}

fn print_type(ty: &Type) -> String {
    match ty {
        Type::SelfType => String::from("@"),
        Type::Name { name, args } => {
            if args.is_empty() {
                return name.clone();
            }
            let args = args.iter().map(print_type).collect::<Vec<_>>().join(", ");
            format!("{name}<{args}>")
        }
        Type::Tuple(elems) => {
            format!("({})", elems.iter().map(print_type).collect::<Vec<_>>().join(", "))
        }
        Type::Func { param, ret } => {
            // `->` is right-associative, so only a function *parameter* type
            // needs bracketing: `(A -> B) -> C` differs from `A -> B -> C`.
            let param = match param.as_ref() {
                nested @ Type::Func { .. } => format!("({})", print_type(nested)),
                other => print_type(other),
            };
            format!("{param} -> {}", print_type(ret))
        }
    }
}

/// Print the right-hand side of a top-level `=` — a function body, a method
/// body, a module constant.
///
/// Items have no terminator, so the body's last token sits directly against the
/// next declaration's first token. Almost every form closes itself; an
/// open-ended range does not, and `f() = ..=` followed by `node() = 1` reads as
/// the range `..=node()`. Parenthesising the body is what the source that
/// produced such an AST must itself have done.
fn print_body(expr: &Expr, depth: usize) -> String {
    let text = print_at(expr, LOOSEST_BP, depth);
    if right_open(&expr.kind) {
        return format!("({text})");
    }
    text
}

/// Whether an expression's printed form ends *expecting more input* — the
/// property that makes it swallow whatever follows. Two tokens do this: a range
/// missing its end (takes the next expression), and a bare `@` (takes the next
/// identifier, because `@name` is receiver-field shorthand — so `let a = @`
/// above `beta() = 1` reads as the field `@beta` and then a stray `(`).
/// Everything else closes with a delimiter or ends on a token no position can
/// extend. The recursion walks the right spine, since that is where the last
/// token comes from.
fn right_open(kind: &ExprKind) -> bool {
    match kind {
        ExprKind::SelfRef => true,
        ExprKind::Range { end, .. } => end.as_ref().is_none_or(|e| right_open(&e.kind)),
        ExprKind::Binary { right, .. } => right_open(&right.kind),
        ExprKind::Unary { operand, .. } => right_open(&operand.kind),
        ExprKind::Lambda { body, .. } => right_open(&body.kind),
        ExprKind::If { els, .. } => right_open(&els.kind),
        _ => false,
    }
}

/// Whether an expression's first token is one the parser recognises as opening
/// an operand. Mirrors `Parser::starts_expr`, which is consulted at exactly one
/// place: deciding whether a `..` has an end. `handle` and `without` head
/// perfectly good expressions but are absent from that list, and a nested
/// prefix range leads with `..`, which is likewise not an operand start.
fn opens_an_operand(kind: &ExprKind) -> bool {
    match kind {
        ExprKind::Handle { .. } | ExprKind::Without { .. } => false,
        ExprKind::Range { start, .. } => start.is_some(),
        ExprKind::Binary { left, .. } => opens_an_operand(&left.kind),
        ExprKind::Call { callee: inner, .. }
        | ExprKind::Field { object: inner, .. }
        | ExprKind::SafeField { object: inner, .. }
        | ExprKind::Index { object: inner, .. }
        | ExprKind::Try(inner) => opens_an_operand(&inner.kind),
        _ => true,
    }
}

/// Whether an infix operator's token can also *begin* an expression — the
/// property that lets an open-ended range on its left reach past it and adopt
/// the right operand as its end. `..  - items` becomes the range `..(-items)`;
/// `.. * items` cannot, because `*` has no prefix meaning.
fn starts_an_expression(op: BinOp) -> bool {
    matches!(op, BinOp::Sub | BinOp::Range | BinOp::RangeIncl)
}

/// Concatenate two printed fragments without letting the lexer re-read the
/// seam as a different token.
///
/// Stitch lexes by maximal munch, so adjacency is not free: `e?` followed by
/// `.name` reads as the single `?.` (safe-navigation) token, and `e?` followed
/// by `..n` fails outright. Everywhere else the printer's own spacing (binary
/// operators) or delimiters (`(`, `[`, `,`) already separate the tokens — `?`
/// before `.` is the one seam that needs a space, and the lexer's token table
/// is what makes that list exactly one entry long.
fn glue(left: &str, right: &str) -> String {
    let munches = left.ends_with('?') && right.starts_with('.');
    let gap = if munches { " " } else { "" };
    format!("{left}{gap}{right}")
}

/// Print `expr` in a position that binds at least as tightly as `min_bp`,
/// wrapping it in parentheses when its own binding power is looser.
fn print_at(expr: &Expr, min_bp: u8, depth: usize) -> String {
    // Inside parentheses the position resets to the loosest — that is what the
    // parentheses buy. Passing `min_bp` on unchanged would make the contents
    // defend against a boundary that is no longer theirs.
    if bp_of(&expr.kind) < min_bp {
        return format!("({})", print_bare(&expr.kind, depth, LOOSEST_BP));
    }
    print_bare(&expr.kind, depth, min_bp)
}

/// How tightly an expression binds *as a whole* — the thing a surrounding
/// position compares against. Only forms the parser can split apart score below
/// [`ATOM_BP`]; anything delimited (a list, a parenthesised tuple, a block) is
/// already closed and needs no help.
fn bp_of(kind: &ExprKind) -> u8 {
    match kind {
        ExprKind::Lambda { .. } | ExprKind::If { .. } => LOOSEST_BP,
        ExprKind::Binary { op, .. } => binding_power(*op).0,
        ExprKind::Range { .. } => binding_power(BinOp::Range).0,
        ExprKind::Unary { .. } | ExprKind::Spread(_) => PREFIX_BP,
        ExprKind::Call { .. }
        | ExprKind::Field { .. }
        | ExprKind::SafeField { .. }
        | ExprKind::Index { .. }
        | ExprKind::Try(_) => POSTFIX_BP,
        _ => ATOM_BP,
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "one arm per AST node; splitting it would scatter the grammar"
)]
fn print_bare(kind: &ExprKind, depth: usize, min_bp: u8) -> String {
    match kind {
        ExprKind::Int(n) => format!("{n}"),
        ExprKind::Float(f) => print_float(*f),
        ExprKind::Bool(b) => String::from(if *b { "true" } else { "false" }),
        ExprKind::Var(name) => name.clone(),
        ExprKind::SelfRef => String::from("@"),
        ExprKind::Placeholder(None) => String::from("$"),
        ExprKind::Placeholder(Some(name)) => format!("${name}"),
        ExprKind::OperatorRef(op) => String::from(binop_text(*op)),
        ExprKind::Spread(base) => format!("..{}", print_at(base, PREFIX_BP, depth)),
        ExprKind::Str(segments) => print_str(segments, depth),
        ExprKind::Unary { op, operand } => {
            let text = String::from(match op {
                UnOp::Neg => "-",
                UnOp::Not => "not ",
            });
            text + &print_at(operand, PREFIX_BP, depth)
        }
        ExprKind::Binary { op, left, right } => {
            let (l_bp, r_bp) = binding_power(*op);
            // A non-associative operator can't have a same-level operand on
            // either side — `(a < b) < c` is the only way to write that tree.
            let left_bp = if is_non_assoc(*op) { r_bp } else { l_bp };
            // An open-ended range (`a..`) has nothing to absorb on its right, so
            // as a *left* operand it needs no parentheses however tightly the
            // operator binds: `a.. * b` already parses as `(a..) * b`. Printing
            // the parentheses anyway is not merely noisy — a statement that
            // begins with `(` is read as a call of the statement before it.
            //
            // It holds only while nothing to the *left* can reach in and take
            // the range's own `..` away. Inside another range's end (`..=(0..)
            // + @`) something can, and there the parentheses are load-bearing.
            let range_bp = binding_power(BinOp::Range).0;
            let safe_bare_range = matches!(left.kind, ExprKind::Range { end: None, .. })
                && min_bp <= range_bp
                && !starts_an_expression(*op);
            // The left operand renders at `min_bp`, not at `left_bp`. In a
            // left-associative climb the parser builds the leftmost operand at
            // whatever level it entered the expression, and only *then* looks
            // for the operator — so `min_bp` is the context its text will
            // actually be read in. `left_bp` decides the parentheses; it is not
            // the position the operand sits in.
            let left_text = if safe_bare_range || bp_of(&left.kind) >= left_bp {
                print_bare(&left.kind, depth, min_bp)
            } else {
                format!("({})", print_bare(&left.kind, depth, LOOSEST_BP))
            };
            format!(
                "{} {} {}",
                left_text,
                binop_text(*op),
                print_at(right, r_bp, depth)
            )
        }
        ExprKind::Range { start, end, inclusive } => {
            let (l_bp, r_bp) = binding_power(BinOp::Range);
            let dots = if *inclusive { "..=" } else { ".." };
            let start = start.as_ref().map_or(String::new(), |e| {
                // The start is whatever the parser had already built when it
                // met the `..`, so it only has to bind tightly enough not to
                // swallow the dots itself — `l_bp`, not `r_bp`. Demanding
                // `r_bp` here parenthesises operands that need no help, and
                // those parentheses go on to break statement juxtaposition.
                // Another range is the exception: `a..b..c` doesn't chain.
                let bp = if matches!(e.kind, ExprKind::Range { .. }) { r_bp } else { l_bp };
                print_at(e, bp, depth)
            });
            let end = end.as_ref().map_or(String::new(), |e| {
                // A range only takes an end when the next token *looks like* an
                // expression start, and the parser's list of those omits
                // `handle`, `without`, and a leading `..`. Written bare, the
                // range closes early and the body becomes a stray declaration.
                if opens_an_operand(&e.kind) {
                    print_at(e, r_bp, depth)
                } else {
                    format!("({})", print_at(e, LOOSEST_BP, depth))
                }
            });
            glue(&start, dots) + &end
        }
        ExprKind::Call { callee, args } => format!(
            "{}({})",
            print_at(callee, POSTFIX_BP, depth),
            print_args(args, depth)
        ),
        ExprKind::Field { object, name } => {
            glue(&print_at(object, POSTFIX_BP, depth), &format!(".{name}"))
        }
        ExprKind::SafeField { object, name } => {
            format!("{}?.{name}", print_at(object, POSTFIX_BP, depth))
        }
        ExprKind::Try(inner) => format!("{}?", print_at(inner, POSTFIX_BP, depth)),
        ExprKind::Index { object, index } => format!(
            "{}[{}]",
            print_at(object, POSTFIX_BP, depth),
            print_at(index, LOOSEST_BP, depth)
        ),
        ExprKind::Lambda { params, body } => {
            // A bare `x -> …` is only a lambda when there is exactly one
            // parameter; `()` and `(a, b)` need their parentheses.
            let params = if params.len() == 1 {
                params[0].clone()
            } else {
                format!("({})", params.join(", "))
            };
            format!("{params} -> {}", print_at(body, LOOSEST_BP, depth))
        }
        ExprKind::If { cond, then, els } => format!(
            "{} => {} | {}",
            print_at(cond, BRANCH_BP, depth),
            print_at(then, BRANCH_BP, depth),
            print_at(els, BRANCH_BP, depth)
        ),
        ExprKind::Tuple(items) => {
            // A one-element tuple would print as `(x)` and re-parse as a
            // parenthesised expression; the trailing comma is what keeps it one.
            let trailing = if items.len() == 1 { "," } else { "" };
            format!("({}{trailing})", print_elems(items, depth))
        }
        ExprKind::List(items) => format!("[{}]", print_elems(items, depth)),
        ExprKind::Map(entries) => {
            if entries.is_empty() {
                return String::from("[:]");
            }
            let body = entries
                .iter()
                .map(|(k, v)| {
                    format!(
                        "{}: {}",
                        print_at(k, LOOSEST_BP, depth),
                        print_at(v, LOOSEST_BP, depth)
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!("[{body}]")
        }
        ExprKind::Block { stmts, result } => print_block(stmts, result.as_deref(), depth),
        ExprKind::Handle { op, handler, body } => format!(
            "handle {op} with {} {}",
            print_at(handler, POSTFIX_BP, depth),
            print_at(body, LOOSEST_BP, depth)
        ),
        ExprKind::Without { cap, body } => {
            format!("without {cap} {}", print_at(body, LOOSEST_BP, depth))
        }
        ExprKind::Match { subject, arms } => {
            let mut out = format!("match {} {{\n", print_at(subject, POSTFIX_BP, depth));
            for arm in arms {
                let guard = arm
                    .guard
                    .as_ref()
                    .map_or(String::new(), |g| format!(" if {}", print_at(g, BRANCH_BP, depth + 1)));
                out.push_str(&indent(depth + 1));
                out.push_str(&print_pattern(&arm.pattern));
                out.push_str(&guard);
                out.push_str(" => ");
                out.push_str(&print_at(&arm.body, BRANCH_BP, depth + 1));
                out.push('\n');
            }
            out.push_str(&indent(depth));
            out.push('}');
            out
        }
        ExprKind::SubjectlessMatch { arms, default } => {
            let mut out = String::from("match {\n");
            for (cond, body) in arms {
                out.push_str(&indent(depth + 1));
                out.push_str(&print_at(cond, BRANCH_BP, depth + 1));
                out.push_str(" => ");
                out.push_str(&print_at(body, BRANCH_BP, depth + 1));
                out.push('\n');
            }
            out.push_str(&indent(depth + 1));
            out.push_str("_ => ");
            out.push_str(&print_at(default, BRANCH_BP, depth + 1));
            out.push('\n');
            out.push_str(&indent(depth));
            out.push('}');
            out
        }
    }
}

/// A `{ … }` block: statements then an optional result, one per line.
fn print_block(stmts: &[Stmt], result: Option<&Expr>, depth: usize) -> String {
    if stmts.is_empty() && result.is_none() {
        return String::from("{ }");
    }
    let mut lines: Vec<String> = stmts
        .iter()
        .map(|stmt| print_stmt(stmt, depth + 1))
        .chain(result.map(|expr| print_at(expr, LOOSEST_BP, depth + 1)))
        .collect();
    seal_seams(&mut lines);

    let mut out = String::from("{\n");
    for line in &lines {
        out.push_str(&indent(depth + 1));
        out.push_str(line);
        out.push('\n');
    }
    out.push_str(&indent(depth));
    out.push('}');
    out
}

/// Parenthesise any statement that would otherwise reach across the newline and
/// eat the next one.
///
/// Statements are separated by whitespace and terminated by nothing, so the
/// boundary between two of them is decided entirely by the tokens either side.
/// `@` before an identifier is the receiver-field shorthand (`@ line` is
/// `@line`); an unfinished range before anything that opens an operand takes it
/// as the range's end.
///
/// Deliberately pairwise rather than a blanket wrap of every risky statement:
/// the parentheses are themselves a hazard, since a statement beginning with
/// `(` reads as a *call* of the statement before it. `{ @ @ }` is two receiver
/// statements and needs no help; `{ @ line }` cannot be written at all. Looking
/// at both sides is what tells those apart.
fn seal_seams(lines: &mut [String]) {
    for i in 0..lines.len().saturating_sub(1) {
        if absorbs(&lines[i], &lines[i + 1]) {
            lines[i] = format!("({})", lines[i]);
        }
    }
}

/// Whether `left`'s last token would swallow `right`'s first.
///
/// Decided on tokens, not characters. `@` before `let` looks like a word
/// boundary but `let` is a keyword, not an identifier, so no field shorthand
/// forms; `..` before `without` looks like an expression but `without` is not
/// something the parser accepts as an operand. Both distinctions are invisible
/// to a character test and both cost a program if guessed wrong.
fn absorbs(left: &str, right: &str) -> bool {
    let Some(next) = crate::lexer::lex(right).tokens.first().map(|t| t.kind.clone()) else {
        return false;
    };
    if left.ends_with('@') {
        return matches!(next, crate::lexer::TokenKind::Ident(_));
    }
    (left.ends_with("..") || left.ends_with("..=")) && crate::parser::token_starts_expr(&next)
}

fn print_stmt(stmt: &Stmt, depth: usize) -> String {
    match stmt {
        Stmt::Let { name, mutable, value } => {
            let mutability = if *mutable { "mut " } else { "" };
            format!("let {mutability}{name} = {}", print_at(value, LOOSEST_BP, depth))
        }
        // The target parses at the loosest precedence (the parser reads a whole
        // expression, then looks for `=`), so it must print there too. Printing
        // it tighter adds parentheses, and a statement that *starts* with `(`
        // is read as a call of the statement before it — juxtaposition is
        // application, and Stitch has no `;` to stop it.
        Stmt::Assign { target, value } => format!(
            "{} = {}",
            print_at(target, LOOSEST_BP, depth),
            print_at(value, LOOSEST_BP, depth)
        ),
        Stmt::Use { binding, call } => {
            let binding = binding.as_ref().map_or(String::new(), |b| format!("{b} "));
            format!("use {binding}<- {}", print_at(call, LOOSEST_BP, depth))
        }
        Stmt::Expr(expr) => print_at(expr, LOOSEST_BP, depth),
    }
}

fn print_pattern(pattern: &Pattern) -> String {
    match pattern {
        Pattern::Wildcard => String::from("_"),
        Pattern::Int(n) => format!("{n}"),
        Pattern::Float(f) => print_float(*f),
        Pattern::Bool(b) => String::from(if *b { "true" } else { "false" }),
        Pattern::Str(text) => format!("\"{}\"", escape(text)),
        Pattern::Binding(name) => name.clone(),
        Pattern::Constructor { name, args } => {
            if args.is_empty() {
                return name.clone();
            }
            let args = args.iter().map(print_pattern).collect::<Vec<_>>().join(", ");
            format!("{name}({args})")
        }
        Pattern::Tuple(elems) => {
            let trailing = if elems.len() == 1 { "," } else { "" };
            format!(
                "({}{trailing})",
                elems.iter().map(print_pattern).collect::<Vec<_>>().join(", ")
            )
        }
        Pattern::Or(alts) => alts.iter().map(print_pattern).collect::<Vec<_>>().join(" | "),
    }
}

fn print_elems(items: &[Expr], depth: usize) -> String {
    items
        .iter()
        .map(|e| print_at(e, LOOSEST_BP, depth))
        .collect::<Vec<_>>()
        .join(", ")
}

fn print_args(args: &[Arg], depth: usize) -> String {
    args.iter()
        .map(|arg| {
            let value = print_at(&arg.value, LOOSEST_BP, depth);
            match &arg.label {
                Some(label) => format!("{label}: {value}"),
                None => value,
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Floats must re-lex as floats: `{}` renders `3.0` as `3`, which comes back an
/// `Int` and silently changes the tree.
fn print_float(f: f64) -> String {
    let text = format!("{f}");
    if text.contains(['.', 'e', 'E', 'n', 'i']) {
        text
    } else {
        text + ".0"
    }
}

fn print_str(segments: &[StrSegment], depth: usize) -> String {
    let mut out = String::from("\"");
    for segment in segments {
        match segment {
            StrSegment::Lit(text) => out.push_str(&escape(text)),
            StrSegment::Interp(expr) => {
                out.push('{');
                out.push_str(&print_at(expr, LOOSEST_BP, depth));
                out.push('}');
            }
        }
    }
    out.push('"');
    out
}

/// Re-escape a literal segment. `{` doubles rather than escapes — a lone `{`
/// would open an interpolation the AST doesn't have.
fn escape(text: &str) -> String {
    let mut out = String::new();
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '\u{1b}' => out.push_str("\\e"),
            '{' => out.push_str("{{"),
            other => out.push(other),
        }
    }
    out
}

fn binop_text(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::Rem => "%",
        BinOp::Eq => "==",
        BinOp::Ne => "!=",
        BinOp::Lt => "<",
        BinOp::Le => "<=",
        BinOp::Gt => ">",
        BinOp::Ge => ">=",
        BinOp::And => "and",
        BinOp::Or => "or",
        BinOp::Pipe => "|>",
        BinOp::CrossPipe => "~>",
        BinOp::Range => "..",
        BinOp::RangeIncl => "..=",
    }
}
