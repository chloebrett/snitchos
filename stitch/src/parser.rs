//! Parser: tokens → AST. A Pratt parser over the §2 precedence table
//! (`plans/lang/01-grammar-and-precedence.md`). Expression grammar so far:
//! literals, variables, unary/binary operators, grouping, and the postfix
//! layer (calls, field access, `?.`, `?`, indexing).

use std::collections::BTreeSet;

use crate::ast::{
    Arg, BinOp, Expr, Field, Item, MatchArm, Method, MethodModifier, Param, Pattern, Stmt,
    StrSegment, Type, UnOp, Variant,
};
use crate::lexer::{StrPart, Token, lex};

/// A parse error. Carries a human-readable message; source positions are a
/// later increment (the lexer doesn't track spans yet).
#[derive(Debug, PartialEq)]
pub struct ParseError {
    pub message: String,
}

impl ParseError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// Stitch has no statement terminators; `;` is lexed but never grammatical.
const NO_SEMICOLONS: &str = "Stitch has no semicolons — remove the `;` (statements are separated by whitespace)";

/// Parse Stitch source into an expression, or return a `ParseError`.
///
/// # Errors
/// Returns `Err` on an unexpected/missing token or trailing input.
pub fn parse(src: &str) -> Result<Expr, ParseError> {
    let mut parser = Parser::new(src);
    let expr = parser.parse_expr(0)?;
    if matches!(parser.peek(), Token::Semicolon) {
        return Err(ParseError::new(NO_SEMICOLONS));
    }
    parser.expect(&Token::Eof, "end of input")?;
    Ok(expr)
}

/// Parse a Stitch program — a sequence of top-level declarations.
///
/// # Errors
/// Returns `Err` on a malformed declaration.
pub fn parse_program(src: &str) -> Result<Vec<Item>, ParseError> {
    let mut parser = Parser::new(src);
    let mut items = Vec::new();
    while !matches!(parser.peek(), Token::Eof) {
        items.push(parser.parse_item()?);
    }
    Ok(items)
}

/// Turn the set of referenced placeholder names (`{"$a", "$c"}`) into a
/// positional lambda parameter list. The letter *is* the index (`$a`=0, `$b`=1,
/// …), so arity is the highest letter referenced and any unreferenced lower slot
/// becomes a `_` hole — letting a placeholder *select* a positional argument
/// (`$b` alone ⇒ `(_, $b)`). Returns `None` when no placeholder was referenced
/// (the argument is an ordinary value, not a lambda).
fn positional_params(referenced: &BTreeSet<String>) -> Option<Vec<String>> {
    let max = referenced
        .iter()
        .filter_map(|name| name.strip_prefix('$').and_then(|s| s.chars().next()))
        .map(|letter| (letter as usize) - ('a' as usize))
        .max()?;
    let params = (0..=max)
        .map(|index| {
            let letter = (b'a' + index as u8) as char;
            let name = format!("${letter}");
            if referenced.contains(&name) {
                name
            } else {
                "_".to_string()
            }
        })
        .collect();
    Some(params)
}

/// Rewrite `Placeholder` nodes in `expr` into `Var("$x")`, collecting the
/// `$x` parameter names used. Stops at explicit `Lambda` boundaries (a
/// placeholder inside a written-out lambda isn't ours to capture). Used to
/// desugar `$`-placeholder arguments into lambdas at the enclosing call.
fn collect_placeholders(expr: &mut Expr, params: &mut BTreeSet<String>) {
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
            for arg in args {
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
            if let Some(start) = start {
                collect_placeholders(start, params);
            }
            if let Some(end) = end {
                collect_placeholders(end, params);
            }
        }
        Expr::If { cond, then, els } => {
            collect_placeholders(cond, params);
            collect_placeholders(then, params);
            collect_placeholders(els, params);
        }
        Expr::Tuple(elems) | Expr::List(elems) => {
            for elem in elems {
                collect_placeholders(elem, params);
            }
        }
        Expr::Map(entries) => {
            for (key, value) in entries {
                collect_placeholders(key, params);
                collect_placeholders(value, params);
            }
        }
        // Atoms with no sub-expressions, explicit lambdas (their body's
        // placeholders belong to that lambda), and strings (interpolations are
        // already sub-parsed) — all left for a later check.
        Expr::Int(_)
        | Expr::Float(_)
        | Expr::Bool(_)
        | Expr::Var(_)
        | Expr::SelfRef
        | Expr::Str(_)
        | Expr::Lambda { .. }
        | Expr::Block { .. }
        | Expr::Match { .. } => {}
    }
}

/// Convert lexer string parts into AST segments, sub-parsing each `{expr}`
/// interpolation's raw source into a full expression.
fn parse_str_segments(parts: Vec<StrPart>) -> Result<Vec<StrSegment>, ParseError> {
    parts
        .into_iter()
        .map(|part| match part {
            StrPart::Lit(text) => Ok(StrSegment::Lit(text)),
            StrPart::Expr(raw) => {
                let inner = parse(&raw).map_err(|e| {
                    ParseError::new(format!("in string interpolation `{{{raw}}}`: {}", e.message))
                })?;
                Ok(StrSegment::Interp(Box::new(inner)))
            }
        })
        .collect()
}

/// Does this identifier start with an uppercase letter? Used to tell a
/// constructor pattern (`Circle`, `None`) from a binding (`x`).
fn starts_uppercase(s: &str) -> bool {
    s.chars().next().is_some_and(char::is_uppercase)
}

/// Map an infix-operator token to its `BinOp`, or `None` if it isn't one.
fn infix_op(tok: &Token) -> Option<BinOp> {
    Some(match tok {
        Token::Plus => BinOp::Add,
        Token::Minus => BinOp::Sub,
        Token::Star => BinOp::Mul,
        Token::Slash => BinOp::Div,
        Token::Percent => BinOp::Rem,
        Token::EqEq => BinOp::Eq,
        Token::NotEq => BinOp::Ne,
        Token::Lt => BinOp::Lt,
        Token::Le => BinOp::Le,
        Token::Gt => BinOp::Gt,
        Token::Ge => BinOp::Ge,
        Token::And => BinOp::And,
        Token::Or => BinOp::Or,
        Token::Pipe => BinOp::Pipe,
        Token::DotDot => BinOp::Range,
        Token::DotDotEq => BinOp::RangeIncl,
        _ => return None,
    })
}

/// If `op` is a range operator, return whether it's inclusive (`..=` vs `..`).
fn range_kind(op: BinOp) -> Option<bool> {
    match op {
        BinOp::Range => Some(false),
        BinOp::RangeIncl => Some(true),
        _ => None,
    }
}

/// Comparisons and ranges are non-associative (§2): chaining them at the same
/// precedence level (`a < b < c`, `1..2..3`) is a parse error, not a nesting.
fn is_non_assoc(op: BinOp) -> bool {
    matches!(
        op,
        BinOp::Eq
            | BinOp::Ne
            | BinOp::Lt
            | BinOp::Le
            | BinOp::Gt
            | BinOp::Ge
            | BinOp::Range
            | BinOp::RangeIncl
    )
}

/// The error shown when a non-associative operator is chained.
fn non_assoc_message(op: BinOp) -> &'static str {
    if range_kind(op).is_some() {
        "ranges don't chain — `a..b..c` is ambiguous; use a single range"
    } else {
        "comparisons don't chain — write `a < b and b < c`, not `a < b < c`"
    }
}

/// `(left, right)` binding powers (§2 precedence table). Loosest = lowest;
/// left < right gives left-associativity.
fn binding_power(op: BinOp) -> (u8, u8) {
    match op {
        BinOp::Or => (1, 2),
        BinOp::And => (3, 4),
        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => (5, 6),
        BinOp::Pipe => (7, 8),
        BinOp::Range | BinOp::RangeIncl => (9, 10),
        BinOp::Add | BinOp::Sub => (11, 12),
        BinOp::Mul | BinOp::Div | BinOp::Rem => (13, 14),
    }
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn new(src: &str) -> Self {
        Self {
            tokens: lex(src),
            pos: 0,
        }
    }

    fn peek(&self) -> &Token {
        &self.tokens[self.pos]
    }

    /// Look `offset` tokens ahead, clamped to the trailing `Eof`.
    fn peek_at(&self, offset: usize) -> &Token {
        let i = (self.pos + offset).min(self.tokens.len() - 1);
        &self.tokens[i]
    }

    /// Return the current token and advance past it; stops at `Eof`.
    fn bump(&mut self) -> &Token {
        let i = self.pos;
        if !matches!(self.tokens[i], Token::Eof) {
            self.pos += 1;
        }
        &self.tokens[i]
    }

    /// Pratt precedence climbing: parse an expression whose infix operators
    /// bind at least as tightly as `min_bp`. Layers (tightest → loosest):
    /// `parse_atom` < `parse_prefix` < this.
    fn parse_expr(&mut self, min_bp: u8) -> Result<Expr, ParseError> {
        if self.at_lambda() {
            return self.parse_lambda();
        }
        let mut left = self.parse_prefix()?;
        while let Some(op) = infix_op(self.peek()) {
            let (l_bp, r_bp) = binding_power(op);
            if l_bp < min_bp {
                break;
            }
            self.bump(); // consume the operator
            left = if let Some(inclusive) = range_kind(op) {
                // `start..` is open-ended when no operand follows the `..`.
                let end = self.starts_expr().then(|| self.parse_expr(r_bp)).transpose()?;
                Expr::Range {
                    start: Some(Box::new(left)),
                    end: end.map(Box::new),
                    inclusive,
                }
            } else {
                Expr::Binary {
                    op,
                    left: Box::new(left),
                    right: Box::new(self.parse_expr(r_bp)?),
                }
            };
            // A second operator at the same precedence level can't chain a
            // non-associative one (`a < b < c`, `1..2..3`).
            if is_non_assoc(op)
                && infix_op(self.peek()).is_some_and(|next| binding_power(next).0 == l_bp)
            {
                return Err(ParseError::new(non_assoc_message(op)));
            }
        }
        // The `cond => then | els` conditional binds looser than any binary
        // operator, so only consider it at the top level (not in operand
        // recursion): `a + b => c | d` is `(a + b) => c | d`.
        if min_bp == 0 && matches!(self.peek(), Token::FatArrow) {
            self.bump(); // =>
            // Branches parse above the conditional's own level (min_bp = 1), so
            // a nested `=>` won't be silently absorbed — it must be parenthesised.
            let then = self.parse_expr(1)?;
            self.expect(&Token::Bar, "'|' in conditional")?;
            let els = self.parse_expr(1)?;
            left = Expr::If {
                cond: Box::new(left),
                then: Box::new(then),
                els: Box::new(els),
            };
            if matches!(self.peek(), Token::FatArrow) {
                return Err(ParseError::new(
                    "chained conditionals aren't allowed — use `match` for more than two cases",
                ));
            }
        }
        Ok(left)
    }

    /// Does an explicit lambda start here? `Ident ->` or `( … ) ->`.
    fn at_lambda(&self) -> bool {
        match self.peek() {
            Token::Ident(_) => matches!(self.peek_at(1), Token::Arrow),
            Token::LParen => self.parens_then_arrow(),
            _ => false,
        }
    }

    /// Scan from the current `(` to its matching `)` and report whether an
    /// `->` follows — i.e. whether this is a lambda param list vs. grouping.
    fn parens_then_arrow(&self) -> bool {
        let mut depth = 0usize;
        for (i, tok) in self.tokens.iter().enumerate().skip(self.pos) {
            match tok {
                Token::LParen => depth += 1,
                Token::RParen => {
                    depth -= 1;
                    if depth == 0 {
                        return matches!(self.tokens.get(i + 1), Some(Token::Arrow));
                    }
                }
                Token::Eof => return false,
                _ => {}
            }
        }
        false
    }

    /// Parse a lambda: `params -> body`. Body is a full expression (loosest),
    /// so lambdas are right-associative (`x -> y -> z` is `x -> (y -> z)`).
    fn parse_lambda(&mut self) -> Result<Expr, ParseError> {
        let params = self.parse_lambda_params()?;
        self.expect(&Token::Arrow, "'->'")?;
        let body = self.parse_expr(0)?;
        Ok(Expr::Lambda {
            params,
            body: Box::new(body),
        })
    }

    /// Parse a lambda's parameters: a bare `name`, or `(name, …)`.
    fn parse_lambda_params(&mut self) -> Result<Vec<String>, ParseError> {
        if !matches!(self.peek(), Token::LParen) {
            return Ok(vec![self.expect_ident("lambda parameter")?]);
        }
        self.bump(); // '('
        let mut params = Vec::new();
        if !matches!(self.peek(), Token::RParen) {
            loop {
                params.push(self.expect_ident("lambda parameter")?);
                if matches!(self.peek(), Token::Comma) {
                    self.bump();
                } else {
                    break;
                }
            }
        }
        self.expect(&Token::RParen, "')' after lambda parameters")?;
        Ok(params)
    }

    /// Prefix unary operators (`-`, `not`) and the open-from-start range forms
    /// (`..n`, `..=n`, bare `..`), binding tighter than any infix. (In call-arg
    /// position a leading `..` is a spread, handled earlier in `parse_arg`.)
    fn parse_prefix(&mut self) -> Result<Expr, ParseError> {
        if matches!(self.peek(), Token::DotDot | Token::DotDotEq) {
            let inclusive = matches!(self.peek(), Token::DotDotEq);
            let (_, r_bp) = binding_power(BinOp::Range);
            self.bump(); // '..' / '..='
            let end = self.starts_expr().then(|| self.parse_expr(r_bp)).transpose()?;
            return Ok(Expr::Range {
                start: None,
                end: end.map(Box::new),
                inclusive,
            });
        }
        let op = match self.peek() {
            Token::Minus => UnOp::Neg,
            Token::Not => UnOp::Not,
            _ => return self.parse_postfix(),
        };
        self.bump(); // consume the operator
        Ok(Expr::Unary {
            op,
            operand: Box::new(self.parse_prefix()?),
        })
    }

    /// Can the current token begin an expression atom? Used to tell an
    /// open-ended range (`n..`) from one with an end operand (`n..m`).
    fn starts_expr(&self) -> bool {
        matches!(
            self.peek(),
            Token::Int(_)
                | Token::Float(_)
                | Token::Bool(_)
                | Token::Ident(_)
                | Token::Str(_)
                | Token::Placeholder(_)
                | Token::LParen
                | Token::LBracket
                | Token::LBrace
                | Token::At
                | Token::Match
                | Token::Minus
                | Token::Not
        )
    }

    /// Postfix operators (call, field, `?.`, `?`, index) — the tightest layer,
    /// left-associative so `a.b.c` and `f(x)(y)` chain.
    fn parse_postfix(&mut self) -> Result<Expr, ParseError> {
        let mut expr = self.parse_atom()?;
        loop {
            // Clone the lookahead token so its borrow ends before we recurse.
            match self.peek().clone() {
                Token::LParen => expr = self.parse_call(expr)?,
                Token::Dot => {
                    self.bump();
                    expr = Expr::Field {
                        object: Box::new(expr),
                        name: self.expect_ident("field name after '.'")?,
                    };
                }
                Token::QuestionDot => {
                    self.bump();
                    expr = Expr::SafeField {
                        object: Box::new(expr),
                        name: self.expect_ident("field name after '?.'")?,
                    };
                }
                Token::Question => {
                    self.bump();
                    expr = Expr::Try(Box::new(expr));
                }
                Token::LBracket => {
                    self.bump();
                    let index = self.parse_expr(0)?;
                    self.expect(&Token::RBracket, "']'")?;
                    expr = Expr::Index {
                        object: Box::new(expr),
                        index: Box::new(index),
                    };
                }
                _ => break,
            }
        }
        Ok(expr)
    }

    /// Parse a call's `(args…)`; the callee is already parsed.
    fn parse_call(&mut self, callee: Expr) -> Result<Expr, ParseError> {
        self.bump(); // '('
        let mut args = Vec::new();
        if !matches!(self.peek(), Token::RParen) {
            loop {
                args.push(self.parse_arg()?);
                if matches!(self.peek(), Token::Comma) {
                    self.bump();
                    if matches!(self.peek(), Token::RParen) {
                        break; // trailing comma
                    }
                } else {
                    break;
                }
            }
        }
        self.expect(&Token::RParen, "')' in call arguments")?;
        Ok(Expr::Call {
            callee: Box::new(callee),
            args,
        })
    }

    /// Parse one call argument: an optional `label:` then a value. The value's
    /// `$`-placeholders are desugared into a wrapping lambda (§3); nested calls
    /// have already captured their own, so placeholders here bind to this call.
    fn parse_arg(&mut self) -> Result<Arg, ParseError> {
        // `..base` — a spread (functional-update base).
        if matches!(self.peek(), Token::DotDot) {
            self.bump();
            let base = self.parse_expr(0)?;
            return Ok(Arg {
                label: None,
                value: Expr::Spread(Box::new(base)),
            });
        }
        let label = if matches!(self.peek(), Token::Ident(_)) && matches!(self.peek_at(1), Token::Colon)
        {
            let name = self.expect_ident("argument label")?;
            self.bump(); // ':'
            Some(name)
        } else {
            None
        };
        let mut value = self.parse_expr(0)?;
        let mut referenced = BTreeSet::new();
        collect_placeholders(&mut value, &mut referenced);
        if let Some(params) = positional_params(&referenced) {
            value = Expr::Lambda {
                params,
                body: Box::new(value),
            };
        }
        Ok(Arg { label, value })
    }

    /// Consume the next token, requiring it to equal `want`, or panic with
    /// context. (The single seam where parse errors will become `Result`.)
    fn expect(&mut self, want: &Token, what: &str) -> Result<(), ParseError> {
        let got = self.bump();
        if got == want {
            Ok(())
        } else {
            Err(ParseError::new(format!("expected {what}, found {got:?}")))
        }
    }

    /// Consume an identifier token, returning its name.
    fn expect_ident(&mut self, what: &str) -> Result<String, ParseError> {
        match self.bump().clone() {
            Token::Ident(name) => Ok(name),
            other => Err(ParseError::new(format!("expected {what}, found {other:?}"))),
        }
    }

    /// Parse a `[…]` collection literal — a list `[a, b]` or a map `[k: v, …]`
    /// (empty list `[]`, empty map `[:]`). The opening `[` is already consumed;
    /// list vs. map is decided by whether the first element is followed by `:`.
    fn parse_collection(&mut self) -> Result<Expr, ParseError> {
        if matches!(self.peek(), Token::RBracket) {
            self.bump();
            return Ok(Expr::List(Vec::new()));
        }
        if matches!(self.peek(), Token::Colon) && matches!(self.peek_at(1), Token::RBracket) {
            self.bump(); // :
            self.bump(); // ]
            return Ok(Expr::Map(Vec::new()));
        }
        let first = self.parse_expr(0)?;
        if matches!(self.peek(), Token::Colon) {
            // map: `first` was a key
            self.bump(); // :
            let value = self.parse_expr(0)?;
            let mut entries = vec![(first, value)];
            while matches!(self.peek(), Token::Comma) {
                self.bump();
                if matches!(self.peek(), Token::RBracket) {
                    break; // trailing comma
                }
                let key = self.parse_expr(0)?;
                self.expect(&Token::Colon, "':' in map entry")?;
                entries.push((key, self.parse_expr(0)?));
            }
            self.expect(&Token::RBracket, "']'")?;
            Ok(Expr::Map(entries))
        } else {
            let mut items = vec![first];
            while matches!(self.peek(), Token::Comma) {
                self.bump();
                if matches!(self.peek(), Token::RBracket) {
                    break; // trailing comma
                }
                items.push(self.parse_expr(0)?);
            }
            self.expect(&Token::RBracket, "']'")?;
            Ok(Expr::List(items))
        }
    }

    /// Parse a block `{ stmt* result? }`. The `{` is already consumed.
    /// Statements are separated by maximal munch (no semicolons); the trailing
    /// expression, if any, is the block's value.
    fn parse_block(&mut self) -> Result<Expr, ParseError> {
        let mut stmts = Vec::new();
        let mut result = None;
        while !matches!(self.peek(), Token::RBrace) {
            if matches!(self.peek(), Token::Eof) {
                return Err(ParseError::new("unterminated block: expected '}'"));
            }
            if matches!(self.peek(), Token::Let) {
                stmts.push(self.parse_let()?);
            } else if matches!(self.peek(), Token::Use) {
                stmts.push(self.parse_use()?);
            } else {
                let expr = self.parse_expr(0)?;
                if matches!(self.peek(), Token::Eq) {
                    self.bump(); // '='
                    let value = self.parse_expr(0)?;
                    stmts.push(Stmt::Assign {
                        target: expr,
                        value,
                    });
                } else if matches!(self.peek(), Token::RBrace) {
                    result = Some(Box::new(expr));
                } else {
                    stmts.push(Stmt::Expr(expr));
                }
            }
        }
        self.bump(); // '}'
        Ok(Expr::Block { stmts, result })
    }

    /// Parse a `use binding? <- call` statement (Gleam-style callback sugar).
    fn parse_use(&mut self) -> Result<Stmt, ParseError> {
        self.bump(); // 'use'
        let binding = if matches!(self.peek(), Token::Ident(_)) {
            Some(self.expect_ident("use binding")?)
        } else {
            None
        };
        self.expect(&Token::LArrow, "'<-' in use")?;
        let call = self.parse_expr(0)?;
        Ok(Stmt::Use { binding, call })
    }

    /// Parse a `let` binding statement: `let mut? name = value`.
    fn parse_let(&mut self) -> Result<Stmt, ParseError> {
        let (name, mutable, value) = self.parse_binding()?;
        Ok(Stmt::Let {
            name,
            mutable,
            value,
        })
    }

    /// Parse the shared core of a binding — `let mut? name = value` — used by
    /// both block-level `let` statements and top-level `let` constants. The
    /// `let` keyword is consumed here.
    fn parse_binding(&mut self) -> Result<(String, bool, Expr), ParseError> {
        self.bump(); // 'let'
        let mutable = matches!(self.peek(), Token::Mut);
        if mutable {
            self.bump();
        }
        let name = self.expect_ident("binding name")?;
        self.expect(&Token::Eq, "'=' in let binding")?;
        let value = self.parse_expr(0)?;
        Ok((name, mutable, value))
    }

    /// Parse one top-level declaration.
    fn parse_item(&mut self) -> Result<Item, ParseError> {
        // Optional `ext` exports the item; items are private to their module by
        // default. It precedes the value-introducing declarations only — `ext`
        // on a `contract` (cross-module conformance) or an `on` block isn't
        // meaningful yet.
        let public = if matches!(self.peek(), Token::Ext) {
            self.bump();
            true
        } else {
            false
        };
        if matches!(self.peek(), Token::Use) {
            if public {
                return Err(ParseError::new(
                    "`ext` applies to declarations, not a `use` import",
                ));
            }
            return self.parse_use_import();
        }
        match self.peek() {
            Token::Prod => self.parse_prod(public),
            Token::Sum => self.parse_sum(public),
            Token::Let => {
                let (name, mutable, value) = self.parse_binding()?;
                Ok(Item::Const {
                    name,
                    mutable,
                    value,
                    public,
                })
            }
            Token::Ident(_) => self.parse_func(public),
            Token::Contract | Token::On if public => Err(ParseError::new(
                "`ext` applies to functions, types, and constants — not `contract`/`on`",
            )),
            Token::Contract => self.parse_contract(),
            Token::On => self.parse_on(),
            other => Err(ParseError::new(format!(
                "expected a declaration, found {other:?}"
            ))),
        }
    }

    /// `use M` (whole-module import) or `use M.{ a, b }` (selective import). The
    /// `.{` after the module name signals a selection list.
    fn parse_use_import(&mut self) -> Result<Item, ParseError> {
        self.bump(); // 'use'
        let module = self.expect_ident("module name after `use`")?;
        let names = if matches!(self.peek(), Token::Dot) {
            self.bump(); // '.'
            self.expect(&Token::LBrace, "'{' for a selective import `use M.{ a, b }`")?;
            let mut names = Vec::new();
            while !matches!(self.peek(), Token::RBrace) {
                names.push(self.expect_ident("imported member name")?);
                if matches!(self.peek(), Token::Comma) {
                    self.bump();
                } else {
                    break;
                }
            }
            self.expect(&Token::RBrace, "'}' to close the import selection")?;
            Some(names)
        } else {
            None
        };
        Ok(Item::Use { module, names })
    }

    /// `on Type { methods }` or `on Type : Contract { methods }`.
    fn parse_on(&mut self) -> Result<Item, ParseError> {
        self.bump(); // 'on'
        let target = self.parse_type()?;
        let contract = if matches!(self.peek(), Token::Colon) {
            self.bump();
            Some(self.parse_type()?)
        } else {
            None
        };
        self.expect(&Token::LBrace, "'{' after on target")?;
        let mut methods = Vec::new();
        while !matches!(self.peek(), Token::RBrace) {
            if matches!(self.peek(), Token::Eof) {
                return Err(ParseError::new("unterminated `on` block: expected '}'"));
            }
            methods.push(self.parse_method(true)?); // on-methods require a body
        }
        self.bump(); // '}'
        Ok(Item::On {
            target,
            contract,
            methods,
        })
    }

    /// `contract Name<generics> { method-signatures }`.
    fn parse_contract(&mut self) -> Result<Item, ParseError> {
        self.bump(); // 'contract'
        let name = self.expect_ident("contract name")?;
        let generics = self.parse_generics()?;
        self.expect(&Token::LBrace, "'{' after contract name")?;
        let mut methods = Vec::new();
        while !matches!(self.peek(), Token::RBrace) {
            if matches!(self.peek(), Token::Eof) {
                return Err(ParseError::new("unterminated contract: expected '}'"));
            }
            methods.push(self.parse_method(false)?);
        }
        self.bump(); // '}'
        Ok(Item::Contract {
            name,
            generics,
            methods,
        })
    }

    /// Parse one method `mod? name(params) -> Ret? body?`. The body is `= expr`
    /// or `{ block }`; when `require_body` is false (contract signatures) it may
    /// be absent (abstract).
    fn parse_method(&mut self, require_body: bool) -> Result<Method, ParseError> {
        let modifier = if matches!(self.peek(), Token::Mut) {
            self.bump();
            MethodModifier::Mut
        } else if matches!(self.peek(), Token::Free) {
            self.bump();
            MethodModifier::Free
        } else {
            MethodModifier::Instance
        };
        let name = self.expect_ident("method name")?;
        self.expect(&Token::LParen, "'(' after method name")?;
        let params = self.parse_params()?;
        let ret = if matches!(self.peek(), Token::Arrow) {
            self.bump();
            Some(self.parse_type()?)
        } else {
            None
        };
        let body = if matches!(self.peek(), Token::Eq | Token::LBrace) {
            Some(self.parse_body()?)
        } else if require_body {
            return Err(ParseError::new("expected '=' or '{' for the method body"));
        } else {
            None
        };
        Ok(Method {
            name,
            modifier,
            params,
            ret,
            body,
        })
    }

    /// A function: `name(params) -> Ret? (= expr | { block })`.
    fn parse_func(&mut self, public: bool) -> Result<Item, ParseError> {
        let name = self.expect_ident("function name")?;
        self.expect(&Token::LParen, "'(' after function name")?;
        let params = self.parse_params()?;
        let ret = if matches!(self.peek(), Token::Arrow) {
            self.bump();
            Some(self.parse_type()?)
        } else {
            None
        };
        let body = self.parse_body()?;
        Ok(Item::Func {
            name,
            params,
            ret,
            body,
            public,
        })
    }

    /// A comma-separated parameter list up to and including `)`. The `(` is
    /// already consumed. Each param is `name` or `name: Type`.
    fn parse_params(&mut self) -> Result<Vec<Param>, ParseError> {
        let mut params = Vec::new();
        if !matches!(self.peek(), Token::RParen) {
            loop {
                let name = self.expect_ident("parameter name")?;
                let ty = if matches!(self.peek(), Token::Colon) {
                    self.bump();
                    Some(self.parse_type()?)
                } else {
                    None
                };
                params.push(Param { name, ty });
                if matches!(self.peek(), Token::Comma) {
                    self.bump();
                    if matches!(self.peek(), Token::RParen) {
                        break; // trailing comma
                    }
                } else {
                    break;
                }
            }
        }
        self.expect(&Token::RParen, "')' after parameters")?;
        Ok(params)
    }

    /// A function/method body: `= expr` or a `{ block }`.
    fn parse_body(&mut self) -> Result<Expr, ParseError> {
        if matches!(self.peek(), Token::Eq) {
            self.bump();
            self.parse_expr(0)
        } else if matches!(self.peek(), Token::LBrace) {
            self.bump();
            self.parse_block()
        } else {
            Err(ParseError::new("expected '=' or '{' for the function body"))
        }
    }

    /// `prod Name<generics>(fields)`.
    fn parse_prod(&mut self, public: bool) -> Result<Item, ParseError> {
        self.bump(); // 'prod'
        let name = self.expect_ident("product type name")?;
        let generics = self.parse_generics()?;
        self.expect(&Token::LParen, "'(' after product name")?;
        let fields = self.parse_fields()?;
        Ok(Item::Prod {
            name,
            generics,
            fields,
            public,
        })
    }

    /// `sum Name<generics> = variant | variant | …`.
    fn parse_sum(&mut self, public: bool) -> Result<Item, ParseError> {
        self.bump(); // 'sum'
        let name = self.expect_ident("sum type name")?;
        let generics = self.parse_generics()?;
        self.expect(&Token::Eq, "'=' in sum declaration")?;
        if matches!(self.peek(), Token::Bar) {
            self.bump(); // optional leading '|'
        }
        let mut variants = vec![self.parse_variant()?];
        while matches!(self.peek(), Token::Bar) {
            self.bump();
            variants.push(self.parse_variant()?);
        }
        Ok(Item::Sum {
            name,
            generics,
            variants,
            public,
        })
    }

    /// A sum variant: `Name` or `Name(fields)`.
    fn parse_variant(&mut self) -> Result<Variant, ParseError> {
        let name = self.expect_ident("variant name")?;
        let fields = if matches!(self.peek(), Token::LParen) {
            self.bump(); // '('
            self.parse_fields()?
        } else {
            Vec::new()
        };
        Ok(Variant { name, fields })
    }

    /// Optional `<T, U, …>` generic parameters.
    fn parse_generics(&mut self) -> Result<Vec<String>, ParseError> {
        if !matches!(self.peek(), Token::Lt) {
            return Ok(Vec::new());
        }
        self.bump(); // '<'
        let mut params = vec![self.expect_ident("type parameter")?];
        while matches!(self.peek(), Token::Comma) {
            self.bump();
            params.push(self.expect_ident("type parameter")?);
        }
        self.expect(&Token::Gt, "'>' to close type parameters")?;
        Ok(params)
    }

    /// A comma-separated field list up to and including `)`. The `(` is
    /// already consumed.
    fn parse_fields(&mut self) -> Result<Vec<Field>, ParseError> {
        let mut fields = Vec::new();
        if !matches!(self.peek(), Token::RParen) {
            loop {
                fields.push(self.parse_field()?);
                if matches!(self.peek(), Token::Comma) {
                    self.bump();
                    if matches!(self.peek(), Token::RParen) {
                        break; // trailing comma
                    }
                } else {
                    break;
                }
            }
        }
        self.expect(&Token::RParen, "')' after fields")?;
        Ok(fields)
    }

    /// One field: `ext? mut? name: Type` (named) or `ext? mut? Type` (positional).
    /// `ext` marks the field exposed on an exported type (`mut` marks it mutable);
    /// both default off, and `ext` precedes `mut` (visibility outermost, as for
    /// items).
    fn parse_field(&mut self) -> Result<Field, ParseError> {
        let public = matches!(self.peek(), Token::Ext);
        if public {
            self.bump();
        }
        let mutable = matches!(self.peek(), Token::Mut);
        if mutable {
            self.bump();
        }
        if matches!(self.peek(), Token::Ident(_)) && matches!(self.peek_at(1), Token::Colon) {
            let name = self.expect_ident("field name")?;
            self.bump(); // ':'
            let ty = self.parse_type()?;
            return Ok(Field {
                name: Some(name),
                mutable,
                ty,
                public,
            });
        }
        let ty = self.parse_type()?;
        Ok(Field {
            name: None,
            mutable,
            ty,
            public,
        })
    }

    /// A type: an atom (named type or parenthesized tuple/grouping), then an
    /// optional `-> ret` (right-associative). A `(A, B) -> C` multi-param
    /// function type is a tuple-typed param.
    fn parse_type(&mut self) -> Result<Type, ParseError> {
        let atom = self.parse_type_atom()?;
        if matches!(self.peek(), Token::Arrow) {
            self.bump();
            Ok(Type::Func {
                param: Box::new(atom),
                ret: Box::new(self.parse_type()?),
            })
        } else {
            Ok(atom)
        }
    }

    /// A type atom: a parenthesized form (`()` unit, `(A)` grouping, `(A, B)`
    /// tuple) or a named type with optional `<…>` arguments.
    fn parse_type_atom(&mut self) -> Result<Type, ParseError> {
        // `@` in type position is the self-type (the receiver's own type). v0
        // parses it (and ignores it, like every type annotation); the type
        // system will give it meaning and restrict it to method signatures.
        if matches!(self.peek(), Token::At) {
            self.bump();
            return Ok(Type::SelfType);
        }
        if !matches!(self.peek(), Token::LParen) {
            return self.parse_type_name();
        }
        self.bump(); // '('
        if matches!(self.peek(), Token::RParen) {
            self.bump();
            return Ok(Type::Tuple(Vec::new()));
        }
        let mut elems = vec![self.parse_type()?];
        while matches!(self.peek(), Token::Comma) {
            self.bump();
            if matches!(self.peek(), Token::RParen) {
                break; // trailing comma
            }
            elems.push(self.parse_type()?);
        }
        self.expect(&Token::RParen, "')' in type")?;
        Ok(if elems.len() == 1 {
            elems.pop().expect("len checked == 1") // `(A)` is grouping, not a tuple
        } else {
            Type::Tuple(elems)
        })
    }

    fn parse_type_name(&mut self) -> Result<Type, ParseError> {
        let name = self.expect_ident("type name")?;
        let args = if matches!(self.peek(), Token::Lt) {
            self.bump(); // '<'
            let mut args = vec![self.parse_type()?];
            while matches!(self.peek(), Token::Comma) {
                self.bump();
                args.push(self.parse_type()?);
            }
            self.expect(&Token::Gt, "'>' to close type arguments")?;
            args
        } else {
            Vec::new()
        };
        Ok(Type::Name { name, args })
    }

    /// Parse `match subject { arm* }`. The `match` keyword is already consumed.
    fn parse_match(&mut self) -> Result<Expr, ParseError> {
        if matches!(self.peek(), Token::LBrace) {
            return self.parse_subjectless_match();
        }
        let subject = self.parse_expr(0)?;
        self.expect(&Token::LBrace, "'{' after match subject")?;
        let mut arms = Vec::new();
        while !matches!(self.peek(), Token::RBrace) {
            if matches!(self.peek(), Token::Eof) {
                return Err(ParseError::new("unterminated match: expected '}'"));
            }
            arms.push(self.parse_match_arm()?);
        }
        self.bump(); // '}'
        Ok(Expr::Match {
            subject: Box::new(subject),
            arms,
        })
    }

    /// Parse the subjectless `match { cond => body … _ => default }` condition
    /// table and desugar it into nested `cond => then | els` conditionals
    /// (`Expr::If`) — it's the N-ary form of the binary conditional. Each arm is
    /// `condition => body`; the table must end in a `_ => …` catch-all, which
    /// becomes the innermost else. The `{` is the current token.
    fn parse_subjectless_match(&mut self) -> Result<Expr, ParseError> {
        self.bump(); // '{'
        let mut arms = Vec::new();
        let default = loop {
            if matches!(self.peek(), Token::RBrace) {
                return Err(ParseError::new(
                    "a subjectless `match` must end in a `_ => …` catch-all",
                ));
            }
            if matches!(self.peek(), Token::Eof) {
                return Err(ParseError::new("unterminated match: expected '}'"));
            }
            if self.at_catch_all() {
                self.bump(); // '_'
                self.bump(); // '=>'
                break self.parse_expr(0)?;
            }
            // min_bp = 1 admits every binary operator but leaves the arm's `=>`
            // for us (the same trick `parse_match_arm` uses for guards).
            let cond = self.parse_expr(1)?;
            self.expect(&Token::FatArrow, "'=>' in match arm")?;
            arms.push((cond, self.parse_expr(0)?));
        };
        if !matches!(self.peek(), Token::RBrace) {
            return Err(ParseError::new(
                "a `_ => …` catch-all must be the last arm of a subjectless match",
            ));
        }
        self.bump(); // '}'
        Ok(arms.into_iter().rev().fold(default, |els, (cond, then)| {
            Expr::If {
                cond: Box::new(cond),
                then: Box::new(then),
                els: Box::new(els),
            }
        }))
    }

    /// Is the parser at a `_ =>` subjectless catch-all arm?
    fn at_catch_all(&self) -> bool {
        matches!(self.peek(), Token::Ident(name) if name == "_")
            && matches!(self.peek_at(1), Token::FatArrow)
    }

    /// Parse one arm: `pattern (if guard)? => body`. Arms are separated by
    /// maximal munch (same as block statements).
    fn parse_match_arm(&mut self) -> Result<MatchArm, ParseError> {
        let pattern = self.parse_pattern()?;
        let guard = if matches!(self.peek(), Token::If) {
            self.bump();
            // min_bp = 1 admits every binary operator but skips the `=>`
            // conditional — whose `=>` is the arm separator here.
            Some(self.parse_expr(1)?)
        } else {
            None
        };
        self.expect(&Token::FatArrow, "'=>' in match arm")?;
        let body = self.parse_expr(0)?;
        Ok(MatchArm {
            pattern,
            guard,
            body,
        })
    }

    /// Parse a pattern, including a top-level or-pattern `a | b | …`.
    fn parse_pattern(&mut self) -> Result<Pattern, ParseError> {
        let first = self.parse_pattern_atom()?;
        if !matches!(self.peek(), Token::Bar) {
            return Ok(first);
        }
        let mut alts = vec![first];
        while matches!(self.peek(), Token::Bar) {
            self.bump();
            alts.push(self.parse_pattern_atom()?);
        }
        Ok(Pattern::Or(alts))
    }

    /// Parse a single (non-or) pattern.
    fn parse_pattern_atom(&mut self) -> Result<Pattern, ParseError> {
        Ok(match self.bump().clone() {
            Token::Int(n) => Pattern::Int(n),
            Token::Float(f) => Pattern::Float(f),
            Token::Bool(b) => Pattern::Bool(b),
            Token::Str(parts) => match parts.as_slice() {
                [StrPart::Lit(text)] => Pattern::Str(text.clone()),
                [] => Pattern::Str(String::new()),
                _ => {
                    return Err(ParseError::new(
                        "string interpolation isn't allowed in a pattern — match on a plain string literal",
                    ));
                }
            },
            Token::Ident(name) if name == "_" => Pattern::Wildcard,
            Token::Ident(name) if starts_uppercase(&name) => {
                let args = if matches!(self.peek(), Token::LParen) {
                    self.bump(); // '('
                    self.parse_pattern_list()?
                } else {
                    Vec::new()
                };
                Pattern::Constructor { name, args }
            }
            Token::Ident(name) => Pattern::Binding(name),
            Token::LParen => {
                let mut pats = self.parse_pattern_list()?;
                match pats.pop() {
                    Some(single) if pats.is_empty() => single, // `(p)` is grouping
                    Some(last) => {
                        pats.push(last);
                        Pattern::Tuple(pats)
                    }
                    None => Pattern::Tuple(Vec::new()),
                }
            }
            other => {
                return Err(ParseError::new(format!(
                    "unexpected token in pattern: {other:?}"
                )));
            }
        })
    }

    /// Parse a comma-separated pattern list up to and including `)`. The `(`
    /// is already consumed.
    fn parse_pattern_list(&mut self) -> Result<Vec<Pattern>, ParseError> {
        let mut pats = Vec::new();
        if !matches!(self.peek(), Token::RParen) {
            loop {
                pats.push(self.parse_pattern()?);
                if matches!(self.peek(), Token::Comma) {
                    self.bump();
                    if matches!(self.peek(), Token::RParen) {
                        break; // trailing comma
                    }
                } else {
                    break;
                }
            }
        }
        self.expect(&Token::RParen, "')' in pattern")?;
        Ok(pats)
    }

    fn parse_atom(&mut self) -> Result<Expr, ParseError> {
        // Clone the leading token so its borrow ends before we recurse.
        Ok(match self.bump().clone() {
            Token::Int(n) => Expr::Int(n),
            Token::Float(f) => Expr::Float(f),
            Token::Bool(b) => Expr::Bool(b),
            Token::Ident(name) => Expr::Var(name),
            Token::Placeholder(name) => Expr::Placeholder(name),
            // `@x` is field `x` on the receiver; bare `@` is the receiver.
            Token::At if matches!(self.peek(), Token::Ident(_)) => Expr::Field {
                object: Box::new(Expr::SelfRef),
                name: self.expect_ident("field name after '@'")?,
            },
            Token::At => Expr::SelfRef,
            Token::Str(parts) => Expr::Str(parse_str_segments(parts)?),
            Token::LParen => {
                // `()` unit, `(e)` grouping, `(e, …)` tuple.
                if matches!(self.peek(), Token::RParen) {
                    self.bump();
                    Expr::Tuple(Vec::new())
                } else {
                    let first = self.parse_expr(0)?;
                    if matches!(self.peek(), Token::Comma) {
                        let mut elems = vec![first];
                        while matches!(self.peek(), Token::Comma) {
                            self.bump();
                            if matches!(self.peek(), Token::RParen) {
                                break; // trailing comma (incl. the `(a,)` 1-tuple)
                            }
                            elems.push(self.parse_expr(0)?);
                        }
                        self.expect(&Token::RParen, "')'")?;
                        Expr::Tuple(elems)
                    } else {
                        self.expect(&Token::RParen, "')'")?;
                        first
                    }
                }
            }
            Token::LBracket => self.parse_collection()?,
            Token::LBrace => self.parse_block()?,
            Token::Match => self.parse_match()?,
            Token::Semicolon => return Err(ParseError::new(NO_SEMICOLONS)),
            other => return Err(ParseError::new(format!("unexpected token: {other:?}"))),
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::ast::{Expr, Item};
    use crate::parser::{parse, parse_program};

    /// Parse an expression, unwrapping — for tests with valid Stitch input.
    fn p(src: &str) -> Expr {
        parse(src).expect("test input should parse")
    }

    /// Parse a program (declarations), unwrapping.
    fn prog(src: &str) -> Vec<Item> {
        parse_program(src).expect("test program should parse")
    }

    #[test]
    fn parses_an_integer_literal() {
        insta::assert_debug_snapshot!(p("42"));
    }

    #[test]
    fn parses_a_float_literal() {
        insta::assert_debug_snapshot!(p("3.14"));
    }

    #[test]
    fn parses_a_bool_literal() {
        insta::assert_debug_snapshot!(p("true"));
    }

    #[test]
    fn parses_a_variable_reference() {
        insta::assert_debug_snapshot!(p("foo"));
    }

    #[test]
    fn parses_addition() {
        insta::assert_debug_snapshot!(p("1 + 2"));
    }

    #[test]
    fn multiplication_binds_tighter_than_addition() {
        insta::assert_debug_snapshot!(p("1 + 2 * 3"));
    }

    #[test]
    fn parentheses_override_precedence() {
        insta::assert_debug_snapshot!(p("(1 + 2) * 3"));
    }

    #[test]
    fn parses_comparison() {
        insta::assert_debug_snapshot!(p("1 < 2"));
    }

    #[test]
    fn addition_binds_tighter_than_comparison() {
        insta::assert_debug_snapshot!(p("1 + 2 < 3"));
    }

    #[test]
    fn and_binds_tighter_than_or() {
        insta::assert_debug_snapshot!(p("a and b or c"));
    }

    #[test]
    fn arithmetic_binds_tighter_than_pipe() {
        insta::assert_debug_snapshot!(p("a + b |> f"));
    }

    #[test]
    fn pipe_binds_tighter_than_comparison() {
        insta::assert_debug_snapshot!(p("x |> f == y"));
    }

    #[test]
    fn addition_binds_tighter_than_range() {
        insta::assert_debug_snapshot!(p("1 .. n + 1"));
    }

    #[test]
    fn parses_negation() {
        insta::assert_debug_snapshot!(p("-x"));
    }

    #[test]
    fn parses_logical_not() {
        insta::assert_debug_snapshot!(p("not a"));
    }

    #[test]
    fn unary_binds_tighter_than_multiplication() {
        insta::assert_debug_snapshot!(p("-x * y"));
    }

    #[test]
    fn not_binds_tighter_than_and() {
        insta::assert_debug_snapshot!(p("not a and b"));
    }

    #[test]
    fn parses_call_with_args() {
        insta::assert_debug_snapshot!(p("f(x, y)"));
    }

    #[test]
    fn parses_empty_call() {
        insta::assert_debug_snapshot!(p("f()"));
    }

    #[test]
    fn chains_field_access() {
        insta::assert_debug_snapshot!(p("a.b.c"));
    }

    #[test]
    fn parses_try() {
        insta::assert_debug_snapshot!(p("x?"));
    }

    #[test]
    fn parses_safe_navigation() {
        insta::assert_debug_snapshot!(p("a?.b"));
    }

    #[test]
    fn parses_index() {
        insta::assert_debug_snapshot!(p("xs[0]"));
    }

    #[test]
    fn postfix_binds_tighter_than_unary() {
        insta::assert_debug_snapshot!(p("-f(x)"));
    }

    #[test]
    fn pipe_with_call() {
        insta::assert_debug_snapshot!(p("readings |> filter(p)"));
    }

    #[test]
    fn unclosed_paren_is_an_error() {
        insta::assert_debug_snapshot!(parse("(1"));
    }

    #[test]
    fn trailing_tokens_are_an_error() {
        insta::assert_debug_snapshot!(parse("1 2"));
    }

    #[test]
    fn an_operator_with_no_operand_is_an_error() {
        insta::assert_debug_snapshot!(parse("1 +"));
    }

    #[test]
    fn parses_single_param_lambda() {
        insta::assert_debug_snapshot!(p("x -> x + 1"));
    }

    #[test]
    fn parses_multi_param_lambda() {
        insta::assert_debug_snapshot!(p("(a, b) -> a + b"));
    }

    #[test]
    fn parses_zero_param_lambda() {
        insta::assert_debug_snapshot!(p("() -> 42"));
    }

    #[test]
    fn parses_ignore_param_lambda() {
        insta::assert_debug_snapshot!(p("_ -> 0"));
    }

    #[test]
    fn lambda_as_call_argument() {
        insta::assert_debug_snapshot!(p("map(x -> x * 2)"));
    }

    #[test]
    fn lambda_is_right_associative() {
        insta::assert_debug_snapshot!(p("x -> y -> z"));
    }

    #[test]
    fn parenthesized_grouping_is_not_a_lambda() {
        insta::assert_debug_snapshot!(p("(1 + 2) * 3"));
    }

    #[test]
    fn placeholder_in_call_becomes_a_lambda() {
        insta::assert_debug_snapshot!(p("map($ * 2)"));
    }

    #[test]
    fn bare_dollar_is_the_first_param() {
        insta::assert_debug_snapshot!(p("each($)"));
    }

    #[test]
    fn two_placeholders_give_arity_two() {
        insta::assert_debug_snapshot!(p("fold(0, $a + $b)"));
    }

    #[test]
    fn repeated_dollar_is_arity_one() {
        insta::assert_debug_snapshot!(p("map($ * $)"));
    }

    #[test]
    fn placeholder_with_field_access() {
        insta::assert_debug_snapshot!(p("map($.celsius)"));
    }

    #[test]
    fn placeholder_gap_becomes_an_ignored_param() {
        insta::assert_debug_snapshot!(p("f($a + $c)"));
    }

    #[test]
    fn placeholder_wraps_only_its_own_argument() {
        insta::assert_debug_snapshot!(p("f($ > 30, other)"));
    }

    #[test]
    fn parses_conditional() {
        insta::assert_debug_snapshot!(p("n < 0 => neg | pos"));
    }

    #[test]
    fn conditional_condition_is_a_full_binary_expression() {
        insta::assert_debug_snapshot!(p("a + b * c => x | y"));
    }

    #[test]
    fn conditional_in_call_argument() {
        insta::assert_debug_snapshot!(p("f(x > 0 => 1 | 0)"));
    }

    #[test]
    fn conditional_without_else_is_an_error() {
        insta::assert_debug_snapshot!(parse("x => a"));
    }

    #[test]
    fn comparisons_do_not_chain() {
        insta::assert_debug_snapshot!(parse("a < b < c"));
    }

    #[test]
    fn mixed_comparisons_do_not_chain() {
        insta::assert_debug_snapshot!(parse("a == b != c"));
    }

    #[test]
    fn ranges_do_not_chain() {
        insta::assert_debug_snapshot!(parse("1..2..3"));
    }

    #[test]
    fn chained_conditionals_point_to_match() {
        insta::assert_debug_snapshot!(parse("a => 1 | b => 2 | 3"));
    }

    #[test]
    fn a_parenthesized_nested_conditional_is_allowed() {
        insta::assert_debug_snapshot!(p("a => 1 | (b => 2 | 3)"));
    }

    #[test]
    fn parses_empty_list() {
        insta::assert_debug_snapshot!(p("[]"));
    }

    #[test]
    fn parses_list_literal() {
        insta::assert_debug_snapshot!(p("[1, 2, 3]"));
    }

    #[test]
    fn parses_empty_map() {
        insta::assert_debug_snapshot!(p("[:]"));
    }

    #[test]
    fn parses_map_literal() {
        insta::assert_debug_snapshot!(p("[a: 1, b: 2]"));
    }

    #[test]
    fn parses_nested_list() {
        insta::assert_debug_snapshot!(p("[[1], [2, 3]]"));
    }

    #[test]
    fn list_literal_distinct_from_indexing() {
        insta::assert_debug_snapshot!(p("[xs[0], ys[1]]"));
    }

    #[test]
    fn parses_plain_string() {
        insta::assert_debug_snapshot!(p(r#""hello""#));
    }

    #[test]
    fn parses_string_interpolation() {
        insta::assert_debug_snapshot!(p(r#""hi {name}!""#));
    }

    #[test]
    fn interpolation_can_hold_an_expression() {
        insta::assert_debug_snapshot!(p(r#""total {a + b}""#));
    }

    #[test]
    fn string_works_as_map_key() {
        insta::assert_debug_snapshot!(p(r#"["host": 1]"#));
    }

    #[test]
    fn parses_block_with_result_only() {
        insta::assert_debug_snapshot!(p("{ 1 + 2 }"));
    }

    #[test]
    fn parses_empty_block() {
        insta::assert_debug_snapshot!(p("{}"));
    }

    #[test]
    fn parses_block_with_let() {
        insta::assert_debug_snapshot!(p("{ let x = 1  x + 2 }"));
    }

    #[test]
    fn parses_block_with_let_mut() {
        insta::assert_debug_snapshot!(p("{ let mut n = 0  n }"));
    }

    #[test]
    fn maximal_munch_separates_statements() {
        insta::assert_debug_snapshot!(p("{ f(x)  g(y)  z }"));
    }

    #[test]
    fn block_as_lambda_body() {
        insta::assert_debug_snapshot!(p("x -> { let y = x * 2  y + 1 }"));
    }

    #[test]
    fn parses_match_with_literals_and_wildcard() {
        insta::assert_debug_snapshot!(p(r#"match n { 0 => "zero"  1 => "one"  _ => "many" }"#));
    }

    #[test]
    fn parses_constructor_patterns() {
        insta::assert_debug_snapshot!(p("match shape { Circle(r) => r  Rect(w, h) => w }"));
    }

    #[test]
    fn parses_nullary_and_unary_constructors() {
        insta::assert_debug_snapshot!(p("match opt { Some(x) => x  None => 0 }"));
    }

    #[test]
    fn parses_or_pattern() {
        insta::assert_debug_snapshot!(p(r#"match n { 1 | 2 | 3 => "small"  _ => "big" }"#));
    }

    #[test]
    fn parses_guard() {
        insta::assert_debug_snapshot!(p(r#"match n { x if x > 0 => "pos"  _ => "neg" }"#));
    }

    #[test]
    fn parses_tuple_pattern() {
        insta::assert_debug_snapshot!(p("match pair { (a, b) => a + b }"));
    }

    #[test]
    fn parses_prod_with_named_fields() {
        insta::assert_debug_snapshot!(prog("prod Point(x: Int, y: Int)"));
    }

    #[test]
    fn parses_prod_with_mut_field() {
        insta::assert_debug_snapshot!(prog("prod Counter(mut n: Int)"));
    }

    #[test]
    fn parses_newtype_prod_positional_field() {
        insta::assert_debug_snapshot!(prog("prod Celsius(Int)"));
    }

    #[test]
    fn parses_sum_with_named_variant_fields() {
        insta::assert_debug_snapshot!(prog(
            "sum Shape = Circle(radius: Int) | Rect(w: Int, h: Int)"
        ));
    }

    #[test]
    fn parses_generic_sum_positional_and_nullary() {
        insta::assert_debug_snapshot!(prog("sum Maybe<T> = Some(T) | None"));
    }

    #[test]
    fn parses_sum_with_leading_bar() {
        insta::assert_debug_snapshot!(prog("sum Color = | Red | Green | Blue"));
    }

    #[test]
    fn parses_multiple_items() {
        insta::assert_debug_snapshot!(prog("prod Point(x: Int, y: Int)  sum Dir = North | South"));
    }

    #[test]
    fn parses_generic_field_type() {
        insta::assert_debug_snapshot!(prog("prod Bag(items: List<Int>, lookup: Map<Str, Int>)"));
    }

    #[test]
    fn parses_expr_body_function() {
        insta::assert_debug_snapshot!(prog("double(x) = x * 2"));
    }

    #[test]
    fn parses_typed_function() {
        insta::assert_debug_snapshot!(prog("add(a: Int, b: Int) -> Int = a + b"));
    }

    #[test]
    fn parses_block_body_function() {
        insta::assert_debug_snapshot!(prog("run() { let x = 1  x + 1 }"));
    }

    #[test]
    fn parses_function_with_return_type() {
        insta::assert_debug_snapshot!(prog("fetch(url: Str) -> Response = get(url)"));
    }

    #[test]
    fn parses_function_among_type_declarations() {
        insta::assert_debug_snapshot!(prog("prod P(x: Int)  area(p) = p.x * 2"));
    }

    #[test]
    fn parses_contract_with_abstract_method() {
        insta::assert_debug_snapshot!(prog("contract Show { show() -> Str }"));
    }

    #[test]
    fn parses_contract_multiple_methods() {
        insta::assert_debug_snapshot!(prog("contract Drawable { draw()  bounds() -> Rect }"));
    }

    #[test]
    fn parses_contract_with_mut_and_free_modifiers() {
        insta::assert_debug_snapshot!(prog("contract Counter { mut bump()  free zero() -> Counter }"));
    }

    #[test]
    fn parses_contract_with_default_method() {
        insta::assert_debug_snapshot!(prog(r#"contract Greet { hello() -> Str = "hi" }"#));
    }

    #[test]
    fn parses_generic_contract() {
        insta::assert_debug_snapshot!(prog("contract Container<T> { get() -> T }"));
    }

    #[test]
    fn parses_self_type_in_a_method_signature() {
        // `@` in type position is the self-type — `unwrap() -> @` returns the
        // receiver's own type. Parsed (and ignored) in v0; meaning arrives with
        // the type system.
        insta::assert_debug_snapshot!(prog("contract Try { unwrap() -> @ }"));
    }

    #[test]
    fn parses_on_block_with_self_fields() {
        insta::assert_debug_snapshot!(prog("on Point { dist() = @x * @x + @y * @y }"));
    }

    #[test]
    fn parses_on_block_implementing_a_contract() {
        insta::assert_debug_snapshot!(prog(r#"on Point : Show { show() = "point" }"#));
    }

    #[test]
    fn parses_on_block_with_modifiers() {
        insta::assert_debug_snapshot!(prog(
            "on Counter { free make() -> Counter = new()  current() -> Int = @n }"
        ));
    }

    #[test]
    fn parses_bare_self_reference() {
        insta::assert_debug_snapshot!(prog("on Box { get() = @ }"));
    }

    #[test]
    fn parses_pair_tuple() {
        insta::assert_debug_snapshot!(p("(1, 2)"));
    }

    #[test]
    fn parses_unit_tuple() {
        insta::assert_debug_snapshot!(p("()"));
    }

    #[test]
    fn grouping_is_not_a_tuple() {
        insta::assert_debug_snapshot!(p("(1 + 2)"));
    }

    #[test]
    fn single_element_tuple_needs_trailing_comma() {
        insta::assert_debug_snapshot!(p("(a,)"));
    }

    #[test]
    fn parses_nested_tuple() {
        insta::assert_debug_snapshot!(p("((1, 2), 3)"));
    }

    #[test]
    fn parses_named_arguments() {
        insta::assert_debug_snapshot!(p("Point(x: 1, y: 2)"));
    }

    #[test]
    fn parses_mixed_positional_and_named_arguments() {
        insta::assert_debug_snapshot!(p("f(a, scale: factor * 2)"));
    }

    #[test]
    fn parses_spread_construction() {
        insta::assert_debug_snapshot!(p("Point(..p, x: 10)"));
    }

    #[test]
    fn parses_self_field_assignment() {
        insta::assert_debug_snapshot!(p("{ @x = 5  @x }"));
    }

    #[test]
    fn parses_local_reassignment() {
        insta::assert_debug_snapshot!(p("{ let mut n = 0  n = n + 1  n }"));
    }

    #[test]
    fn parses_use_with_binding() {
        insta::assert_debug_snapshot!(p("{ use r <- each(readings)  emit(r) }"));
    }

    #[test]
    fn parses_use_without_binding() {
        insta::assert_debug_snapshot!(p(r#"{ use <- span("report")  emit(x) }"#));
    }

    #[test]
    fn a_stray_semicolon_is_a_helpful_error() {
        insta::assert_debug_snapshot!(parse("1; 2"));
    }

    #[test]
    fn an_error_inside_interpolation_names_its_context() {
        insta::assert_debug_snapshot!(parse(r#""value is {1 +}""#));
    }

    #[test]
    fn parses_float_literal_pattern() {
        insta::assert_debug_snapshot!(p(r#"match x { 3.14 => "pi"  _ => "other" }"#));
    }

    #[test]
    fn parses_string_literal_pattern() {
        insta::assert_debug_snapshot!(p(r#"match s { "hi" => 1  _ => 0 }"#));
    }

    #[test]
    fn an_interpolated_string_pattern_is_an_error() {
        insta::assert_debug_snapshot!(parse(r#"match s { "{x}" => 1  _ => 0 }"#));
    }

    #[test]
    fn parses_top_level_constant() {
        insta::assert_debug_snapshot!(prog("let pi = 3.14"));
    }

    #[test]
    fn parses_top_level_mut_constant() {
        insta::assert_debug_snapshot!(prog("let mut counter = 0"));
    }

    #[test]
    fn parses_constant_among_declarations() {
        insta::assert_debug_snapshot!(prog("let limit = 100  area(p) = p.x * limit"));
    }

    #[test]
    fn parses_whole_module_and_selective_imports() {
        let whole = prog("use math");
        assert!(matches!(&whole[0], Item::Use { module, names: None } if module == "math"));
        let selective = prog("use math.{double, area}");
        let Item::Use { module, names: Some(names) } = &selective[0] else {
            panic!("expected a selective import");
        };
        assert_eq!(module, "math");
        assert_eq!(names.as_slice(), ["double".to_string(), "area".to_string()]);
    }

    #[test]
    fn ext_marks_an_item_as_exported() {
        // Items are private by default; `ext` flips the flag the export table reads.
        let private = prog("area(p) = p.x");
        assert!(matches!(&private[0], Item::Func { public: false, .. }));
        let exported = prog("ext area(p) = p.x");
        assert!(matches!(&exported[0], Item::Func { public: true, .. }));
    }

    #[test]
    fn ext_on_a_contract_is_a_parse_error() {
        // `ext` is for value-introducing items; contract/`on` visibility isn't
        // meaningful yet, so it's rejected rather than silently ignored.
        let error = parse_program("ext contract Show { show() -> Str }")
            .expect_err("`ext contract` should be rejected");
        assert!(
            error.message.contains("`ext` applies to"),
            "{}",
            error.message
        );
    }

    #[test]
    fn parses_subjectless_match_as_nested_conditionals() {
        insta::assert_debug_snapshot!(p(r#"match { n > 10 => "big"  n > 0 => "small"  _ => "neg" }"#));
    }

    #[test]
    fn subjectless_match_with_only_catch_all_is_the_default() {
        insta::assert_debug_snapshot!(p("match { _ => 0 }"));
    }

    #[test]
    fn subjectless_match_requires_a_catch_all() {
        insta::assert_debug_snapshot!(parse(r#"match { x > 0 => "pos" }"#));
    }

    #[test]
    fn subjectless_match_rejects_arms_after_catch_all() {
        insta::assert_debug_snapshot!(parse("match { _ => 0  x > 0 => 1 }"));
    }

    #[test]
    fn parses_tuple_type_annotation() {
        insta::assert_debug_snapshot!(prog("prod Pair(items: (Int, Str))"));
    }

    #[test]
    fn parses_multi_param_function_type() {
        insta::assert_debug_snapshot!(prog("prod Handler(cb: (Int, Str) -> Bool)"));
    }

    #[test]
    fn parses_closed_range() {
        insta::assert_debug_snapshot!(p("1..10"));
    }

    #[test]
    fn parses_inclusive_range() {
        insta::assert_debug_snapshot!(p("0..=n"));
    }

    #[test]
    fn parses_open_ended_range_from() {
        insta::assert_debug_snapshot!(p("n.."));
    }

    #[test]
    fn parses_range_to() {
        insta::assert_debug_snapshot!(p("..n"));
    }

    #[test]
    fn parses_inclusive_range_to() {
        insta::assert_debug_snapshot!(p("..=n"));
    }

    #[test]
    fn open_range_feeds_a_pipeline() {
        insta::assert_debug_snapshot!(p("n.. |> take(5)"));
    }

    #[test]
    fn parses_thunk_type() {
        insta::assert_debug_snapshot!(prog("prod Lazy(run: () -> Int)"));
    }

    #[test]
    fn parenthesized_type_is_not_a_tuple() {
        insta::assert_debug_snapshot!(prog("prod Wrap(x: (Int))"));
    }
}
