//! Parser: tokens → AST. A Pratt parser over the §2 precedence table
//! (`plans/lang/01-grammar-and-precedence.md`). Expression grammar so far:
//! literals, variables, unary/binary operators, grouping, and the postfix
//! layer (calls, field access, `?.`, `?`, indexing).


#[allow(clippy::wildcard_imports, reason = "alloc prelude for no_std")]
use crate::prelude::*;

use crate::ast::{
    Arg, BinOp, Effect, Expr, ExprKind, Field, Item, MatchArm, Method, MethodModifier, Param,
    Pattern, Stmt, StrSegment, Type, UnOp, Variant,
};
use crate::lexer::{LexError, Span, StrPart, Token, TokenKind, lex};

/// A parse error: a human-readable message plus the source [`Span`] it points
/// at (defaulted to the start of input for errors not yet span-attributed).
#[derive(Debug, PartialEq)]
pub struct ParseError {
    pub message: String,
    pub span: Span,
}

impl ParseError {
    /// A parse error anchored at `span`.
    fn at(message: impl Into<String>, span: Span) -> Self {
        Self { message: message.into(), span }
    }

    /// Render as `line:col: message`, followed by the offending source line and
    /// a caret under the span's start. Line and (character-counted) column are
    /// 1-based.
    #[must_use]
    pub fn render(&self, src: &str) -> String {
        crate::source::caret_render(src, self.span, &self.message)
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
    if let Some(err) = parser.lex_error() {
        return Err(err);
    }
    let expr = parser.parse_expr(0)?;
    if matches!(parser.peek(), TokenKind::Semicolon) {
        return Err(parser.err(NO_SEMICOLONS));
    }
    parser.expect(&TokenKind::Eof, "end of input")?;
    Ok(expr)
}

/// Parse a Stitch program — a sequence of top-level declarations.
///
/// # Errors
/// Returns `Err` on a malformed declaration.
pub fn parse_program(src: &str) -> Result<Vec<Item>, ParseError> {
    let mut parser = Parser::new(src);
    if let Some(err) = parser.lex_error() {
        return Err(err);
    }
    let mut items = Vec::new();
    while !matches!(parser.peek(), TokenKind::Eof) {
        items.push(parser.parse_item()?);
    }
    Ok(items)
}


/// Convert lexer string parts into AST segments, sub-parsing each `{expr}`
/// interpolation's raw source into a full expression.
fn parse_str_segments(parts: Vec<StrPart>) -> Result<Vec<StrSegment>, ParseError> {
    parts
        .into_iter()
        .map(|part| match part {
            StrPart::Lit(text) => Ok(StrSegment::Lit(text)),
            StrPart::Expr(tokens, lex_errors) => {
                let mut sub = Parser::from_tokens(tokens, lex_errors);
                if let Some(err) = sub.lex_error() {
                    return Err(sub.err(format!("in string interpolation: {}", err.message)));
                }
                let inner = sub.parse_expr(0).map_err(|e| {
                    ParseError::at(format!("in string interpolation: {}", e.message), e.span)
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
fn infix_op(tok: &TokenKind) -> Option<BinOp> {
    Some(match tok {
        TokenKind::Plus => BinOp::Add,
        TokenKind::Minus => BinOp::Sub,
        TokenKind::Star => BinOp::Mul,
        TokenKind::Slash => BinOp::Div,
        TokenKind::Percent => BinOp::Rem,
        TokenKind::EqEq => BinOp::Eq,
        TokenKind::NotEq => BinOp::Ne,
        TokenKind::Lt => BinOp::Lt,
        TokenKind::Le => BinOp::Le,
        TokenKind::Gt => BinOp::Gt,
        TokenKind::Ge => BinOp::Ge,
        TokenKind::And => BinOp::And,
        TokenKind::Or => BinOp::Or,
        TokenKind::Pipe => BinOp::Pipe,
        TokenKind::CrossPipe => BinOp::CrossPipe,
        TokenKind::DotDot => BinOp::Range,
        TokenKind::DotDotEq => BinOp::RangeIncl,
        _ => return None,
    })
}

/// A bare binary operator usable as a *value* — the arithmetic, comparison, and
/// logical operators. Excludes the pipe and range operators, which aren't binary
/// value functions. Feeds the operator-as-function sugar in `parse_arg`.
fn operator_fn(tok: &TokenKind) -> Option<BinOp> {
    match infix_op(tok)? {
        BinOp::Pipe | BinOp::CrossPipe | BinOp::Range | BinOp::RangeIncl => None,
        op => Some(op),
    }
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
        BinOp::Pipe | BinOp::CrossPipe => (7, 8),
        BinOp::Range | BinOp::RangeIncl => (9, 10),
        BinOp::Add | BinOp::Sub => (11, 12),
        BinOp::Mul | BinOp::Div | BinOp::Rem => (13, 14),
    }
}

struct Parser {
    tokens: Vec<Token>,
    lex_errors: Vec<LexError>,
    pos: usize,
}

impl Parser {
    fn new(src: &str) -> Self {
        let lexed = lex(src);
        Self {
            tokens: lexed.tokens,
            lex_errors: lexed.errors,
            pos: 0,
        }
    }

    /// Construct a parser directly from pre-lexed tokens (used by `parse_str_segments`
    /// to avoid re-lexing string interpolations).
    fn from_tokens(tokens: Vec<Token>, lex_errors: Vec<LexError>) -> Self {
        Self { tokens, lex_errors, pos: 0 }
    }

    /// The first lexing error (if any), as a parse error — so malformed input is
    /// surfaced instead of silently miscompiling.
    fn lex_error(&self) -> Option<ParseError> {
        self.lex_errors
            .first()
            .map(|e| ParseError::at(e.message.clone(), e.span))
    }

    fn peek(&self) -> &TokenKind {
        &self.tokens[self.pos].kind
    }

    fn current_span(&self) -> Span {
        self.tokens[self.pos].span
    }

    fn err(&self, message: impl Into<String>) -> ParseError {
        ParseError::at(message, self.current_span())
    }

    /// The start byte of the current (not-yet-consumed) token — captured at the
    /// entry of a node to mark where its span begins.
    fn cur_start(&self) -> usize {
        self.tokens[self.pos].span.start
    }

    /// The end byte of the most recently consumed token — where the span of a
    /// just-parsed node ends. Zero before anything is consumed.
    fn prev_end(&self) -> usize {
        self.pos.checked_sub(1).map_or(0, |i| self.tokens[i].span.end)
    }

    /// Build an expression node spanning from `start` to the end of the last
    /// consumed token.
    fn spanned(&self, start: usize, kind: ExprKind) -> Expr {
        Expr::new(kind, Span { start, end: self.prev_end() })
    }

    /// Look `offset` tokens ahead, clamped to the trailing `Eof`.
    fn peek_at(&self, offset: usize) -> &TokenKind {
        let i = (self.pos + offset).min(self.tokens.len() - 1);
        &self.tokens[i].kind
    }

    /// Return the current token and advance past it; stops at `Eof`.
    fn bump(&mut self) -> &TokenKind {
        let i = self.pos;
        if !matches!(self.tokens[i].kind, TokenKind::Eof) {
            self.pos += 1;
        }
        &self.tokens[i].kind
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
            let start = left.span.start;
            left = if let Some(inclusive) = range_kind(op) {
                // `start..` is open-ended when no operand follows the `..`.
                let end = self.starts_expr().then(|| self.parse_expr(r_bp)).transpose()?;
                self.spanned(start, ExprKind::Range {
                    start: Some(Box::new(left)),
                    end: end.map(Box::new),
                    inclusive,
                })
            } else {
                let right = self.parse_expr(r_bp)?;
                self.spanned(start, ExprKind::Binary {
                    op,
                    left: Box::new(left),
                    right: Box::new(right),
                })
            };
            // A second operator at the same precedence level can't chain a
            // non-associative one (`a < b < c`, `1..2..3`).
            if is_non_assoc(op)
                && infix_op(self.peek()).is_some_and(|next| binding_power(next).0 == l_bp)
            {
                return Err(self.err(non_assoc_message(op)));
            }
        }
        // The `cond => then | els` conditional binds looser than any binary
        // operator, so only consider it at the top level (not in operand
        // recursion): `a + b => c | d` is `(a + b) => c | d`.
        if min_bp == 0 && matches!(self.peek(), TokenKind::FatArrow) {
            self.bump(); // =>
            // Branches parse above the conditional's own level (min_bp = 1), so
            // a nested `=>` won't be silently absorbed — it must be parenthesised.
            let then = self.parse_expr(1)?;
            self.expect(&TokenKind::Bar, "'|' in conditional")?;
            let els = self.parse_expr(1)?;
            let start = left.span.start;
            left = self.spanned(start, ExprKind::If {
                cond: Box::new(left),
                then: Box::new(then),
                els: Box::new(els),
            });
            if matches!(self.peek(), TokenKind::FatArrow) {
                return Err(self.err(
                    "chained conditionals aren't allowed — use `match` for more than two cases",
                ));
            }
        }
        Ok(left)
    }

    /// Does an explicit lambda start here? `Ident ->` or `( … ) ->`.
    ///
    /// For the parenthesised form, uses checkpoint/backtrack: save pos, try to
    /// parse the parameter list, check for `->`, then unconditionally restore —
    /// O(params) instead of O(token-stream).
    fn at_lambda(&mut self) -> bool {
        match self.peek() {
            TokenKind::Ident(_) => matches!(self.peek_at(1), TokenKind::Arrow),
            TokenKind::LParen => {
                let saved = self.pos;
                let is_lambda = self.parse_lambda_params().is_ok()
                    && matches!(self.peek(), TokenKind::Arrow);
                self.pos = saved;
                is_lambda
            }
            _ => false,
        }
    }

    /// Parse a lambda: `params -> body`. Body is a full expression (loosest),
    /// so lambdas are right-associative (`x -> y -> z` is `x -> (y -> z)`).
    fn parse_lambda(&mut self) -> Result<Expr, ParseError> {
        let start = self.cur_start();
        let params = self.parse_lambda_params()?;
        self.expect(&TokenKind::Arrow, "'->'")?;
        let body = self.parse_expr(0)?;
        Ok(self.spanned(start, ExprKind::Lambda {
            params,
            body: Box::new(body),
        }))
    }

    /// Parse a lambda's parameters: a bare `name`, or `(name, …)`.
    fn parse_lambda_params(&mut self) -> Result<Vec<String>, ParseError> {
        if !matches!(self.peek(), TokenKind::LParen) {
            return Ok(vec![self.expect_ident("lambda parameter")?]);
        }
        self.bump(); // '('
        let mut params = Vec::new();
        if !matches!(self.peek(), TokenKind::RParen) {
            loop {
                params.push(self.expect_ident("lambda parameter")?);
                if matches!(self.peek(), TokenKind::Comma) {
                    self.bump();
                } else {
                    break;
                }
            }
        }
        self.expect(&TokenKind::RParen, "')' after lambda parameters")?;
        Ok(params)
    }

    /// Prefix unary operators (`-`, `not`) and the open-from-start range forms
    /// (`..n`, `..=n`, bare `..`), binding tighter than any infix. (In call-arg
    /// position a leading `..` is a spread, handled earlier in `parse_arg`.)
    fn parse_prefix(&mut self) -> Result<Expr, ParseError> {
        let start = self.cur_start();
        if matches!(self.peek(), TokenKind::DotDot | TokenKind::DotDotEq) {
            let inclusive = matches!(self.peek(), TokenKind::DotDotEq);
            let (_, r_bp) = binding_power(BinOp::Range);
            self.bump(); // '..' / '..='
            let end = self.starts_expr().then(|| self.parse_expr(r_bp)).transpose()?;
            return Ok(self.spanned(start, ExprKind::Range {
                start: None,
                end: end.map(Box::new),
                inclusive,
            }));
        }
        let op = match self.peek() {
            TokenKind::Minus => UnOp::Neg,
            TokenKind::Not => UnOp::Not,
            _ => return self.parse_postfix(),
        };
        self.bump(); // consume the operator
        let operand = self.parse_prefix()?;
        Ok(self.spanned(start, ExprKind::Unary {
            op,
            operand: Box::new(operand),
        }))
    }

    /// Can the current token begin an expression atom? Used to tell an
    /// open-ended range (`n..`) from one with an end operand (`n..m`).
    fn starts_expr(&self) -> bool {
        matches!(
            self.peek(),
            TokenKind::Int(_)
                | TokenKind::Float(_)
                | TokenKind::Bool(_)
                | TokenKind::Ident(_)
                | TokenKind::Str(_)
                | TokenKind::Placeholder(_)
                | TokenKind::LParen
                | TokenKind::LBracket
                | TokenKind::LBrace
                | TokenKind::At
                | TokenKind::Match
                | TokenKind::Minus
                | TokenKind::Not
        )
    }

    /// Postfix operators (call, field, `?.`, `?`, index) — the tightest layer,
    /// left-associative so `a.b.c` and `f(x)(y)` chain.
    fn parse_postfix(&mut self) -> Result<Expr, ParseError> {
        let mut expr = self.parse_atom()?;
        let start = expr.span.start;
        loop {
            // Clone the lookahead token so its borrow ends before we recurse.
            match self.peek().clone() {
                TokenKind::LParen => expr = self.parse_call(start, expr)?,
                TokenKind::Dot => {
                    self.bump();
                    let name = self.expect_ident("field name after '.'")?;
                    expr = self.spanned(start, ExprKind::Field {
                        object: Box::new(expr),
                        name,
                    });
                }
                TokenKind::QuestionDot => {
                    self.bump();
                    let name = self.expect_ident("field name after '?.'")?;
                    expr = self.spanned(start, ExprKind::SafeField {
                        object: Box::new(expr),
                        name,
                    });
                }
                TokenKind::Question => {
                    self.bump();
                    expr = self.spanned(start, ExprKind::Try(Box::new(expr)));
                }
                TokenKind::LBracket => {
                    self.bump();
                    let index = self.parse_expr(0)?;
                    self.expect(&TokenKind::RBracket, "']'")?;
                    expr = self.spanned(start, ExprKind::Index {
                        object: Box::new(expr),
                        index: Box::new(index),
                    });
                }
                _ => break,
            }
        }
        Ok(expr)
    }

    /// Parse a call's `(args…)`; the callee is already parsed. `start` is the
    /// byte offset of the callee (so the call node spans callee through `)`).
    fn parse_call(&mut self, start: usize, callee: Expr) -> Result<Expr, ParseError> {
        self.bump(); // '('
        let mut args = Vec::new();
        if !matches!(self.peek(), TokenKind::RParen) {
            loop {
                args.push(self.parse_arg()?);
                if matches!(self.peek(), TokenKind::Comma) {
                    self.bump();
                    if matches!(self.peek(), TokenKind::RParen) {
                        break; // trailing comma
                    }
                } else {
                    break;
                }
            }
        }
        self.expect(&TokenKind::RParen, "')' in call arguments")?;
        Ok(self.spanned(start, ExprKind::Call {
            callee: Box::new(callee),
            args,
        }))
    }

    /// Parse one call argument: an optional `label:` then a value. The value's
    /// `$`-placeholders are desugared into a wrapping lambda (§3); nested calls
    /// have already captured their own, so placeholders here bind to this call.
    fn parse_arg(&mut self) -> Result<Arg, ParseError> {
        // `..base` — a spread (functional-update base).
        if matches!(self.peek(), TokenKind::DotDot) {
            let start = self.cur_start();
            self.bump();
            let base = self.parse_expr(0)?;
            return Ok(Arg {
                label: None,
                value: self.spanned(start, ExprKind::Spread(Box::new(base))),
            });
        }
        let label = if matches!(self.peek(), TokenKind::Ident(_)) && matches!(self.peek_at(1), TokenKind::Colon)
        {
            let name = self.expect_ident("argument label")?;
            self.bump(); // ':'
            Some(name)
        } else {
            None
        };
        // A bare binary operator in argument position is its function:
        // `fold(0, +)` ≡ `fold(0, (lhs, rhs) -> lhs + rhs)`. Recognised when the
        // operator is immediately followed by an argument terminator (`,` or `)`),
        // which a real binary expression never is.
        if let Some(op) = operator_fn(self.peek())
            && matches!(self.peek_at(1), TokenKind::Comma | TokenKind::RParen)
        {
            let start = self.cur_start();
            self.bump(); // the operator
            return Ok(Arg { label, value: self.spanned(start, ExprKind::OperatorRef(op)) });
        }
        let value = self.parse_expr(0)?;
        Ok(Arg { label, value })
    }

    /// Consume the next token, requiring it to equal `want`, or panic with
    /// context. (The single seam where parse errors will become `Result`.)
    fn expect(&mut self, want: &TokenKind, what: &str) -> Result<(), ParseError> {
        let span = self.tokens[self.pos].span;
        let got = self.bump();
        if got == want {
            Ok(())
        } else {
            Err(ParseError::at(format!("expected {what}"), span))
        }
    }

    /// Consume an identifier token, returning its name.
    fn expect_ident(&mut self, what: &str) -> Result<String, ParseError> {
        let span = self.tokens[self.pos].span;
        match self.bump().clone() {
            TokenKind::Ident(name) => Ok(name),
            _ => Err(ParseError::at(format!("expected {what}"), span)),
        }
    }

    /// Parse a `[…]` collection literal — a list `[a, b]` or a map `[k: v, …]`
    /// (empty list `[]`, empty map `[:]`). The opening `[` is already consumed;
    /// list vs. map is decided by whether the first element is followed by `:`.
    fn parse_collection(&mut self, start: usize) -> Result<Expr, ParseError> {
        if matches!(self.peek(), TokenKind::RBracket) {
            self.bump();
            return Ok(self.spanned(start, ExprKind::List(Vec::new())));
        }
        if matches!(self.peek(), TokenKind::Colon) && matches!(self.peek_at(1), TokenKind::RBracket) {
            self.bump(); // :
            self.bump(); // ]
            return Ok(self.spanned(start, ExprKind::Map(Vec::new())));
        }
        let first = self.parse_expr(0)?;
        if matches!(self.peek(), TokenKind::Colon) {
            // map: `first` was a key
            self.bump(); // :
            let value = self.parse_expr(0)?;
            let mut entries = vec![(first, value)];
            while matches!(self.peek(), TokenKind::Comma) {
                self.bump();
                if matches!(self.peek(), TokenKind::RBracket) {
                    break; // trailing comma
                }
                let key = self.parse_expr(0)?;
                self.expect(&TokenKind::Colon, "':' in map entry")?;
                entries.push((key, self.parse_expr(0)?));
            }
            self.expect(&TokenKind::RBracket, "']'")?;
            Ok(self.spanned(start, ExprKind::Map(entries)))
        } else {
            let mut items = vec![first];
            while matches!(self.peek(), TokenKind::Comma) {
                self.bump();
                if matches!(self.peek(), TokenKind::RBracket) {
                    break; // trailing comma
                }
                items.push(self.parse_expr(0)?);
            }
            self.expect(&TokenKind::RBracket, "']'")?;
            Ok(self.spanned(start, ExprKind::List(items)))
        }
    }

    /// Parse a block `{ stmt* result? }`. The `{` is already consumed; `start` is
    /// the byte offset of that `{`.
    /// Statements are separated by maximal munch (no semicolons); the trailing
    /// expression, if any, is the block's value.
    fn parse_block(&mut self, start: usize) -> Result<Expr, ParseError> {
        let mut stmts = Vec::new();
        let mut result = None;
        while !matches!(self.peek(), TokenKind::RBrace) {
            if matches!(self.peek(), TokenKind::Eof) {
                return Err(self.err("unterminated block: expected '}'"));
            }
            if matches!(self.peek(), TokenKind::Let) {
                stmts.push(self.parse_let()?);
            } else if matches!(self.peek(), TokenKind::Use) {
                stmts.push(self.parse_use()?);
            } else {
                let expr = self.parse_expr(0)?;
                if matches!(self.peek(), TokenKind::Eq) {
                    self.bump(); // '='
                    let value = self.parse_expr(0)?;
                    stmts.push(Stmt::Assign {
                        target: expr,
                        value,
                    });
                } else if matches!(self.peek(), TokenKind::RBrace) {
                    result = Some(Box::new(expr));
                } else {
                    stmts.push(Stmt::Expr(expr));
                }
            }
        }
        self.bump(); // '}'
        Ok(self.spanned(start, ExprKind::Block { stmts, result }))
    }

    /// Parse a `use binding? <- call` statement (Gleam-style callback sugar).
    fn parse_use(&mut self) -> Result<Stmt, ParseError> {
        self.bump(); // 'use'
        let binding = if matches!(self.peek(), TokenKind::Ident(_)) {
            Some(self.expect_ident("use binding")?)
        } else {
            None
        };
        self.expect(&TokenKind::LArrow, "'<-' in use")?;
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
        let mutable = matches!(self.peek(), TokenKind::Mut);
        if mutable {
            self.bump();
        }
        let name = self.expect_ident("binding name")?;
        self.expect(&TokenKind::Eq, "'=' in let binding")?;
        let value = self.parse_expr(0)?;
        Ok((name, mutable, value))
    }

    /// Parse one top-level declaration.
    fn parse_item(&mut self) -> Result<Item, ParseError> {
        // Optional `ext` exports the item; items are private to their module by
        // default. It precedes the value-introducing declarations only — `ext`
        // on a `contract` (cross-module conformance) or an `on` block isn't
        // meaningful yet.
        let public = if matches!(self.peek(), TokenKind::Ext) {
            self.bump();
            true
        } else {
            false
        };
        if matches!(self.peek(), TokenKind::Use) {
            if public {
                return Err(self.err(
                    "`ext` applies to declarations, not a `use` import",
                ));
            }
            return self.parse_use_import();
        }
        match self.peek() {
            TokenKind::Prod => self.parse_prod(public),
            TokenKind::Sum => self.parse_sum(public),
            TokenKind::Let => {
                let (name, mutable, value) = self.parse_binding()?;
                Ok(Item::Const {
                    name,
                    mutable,
                    value,
                    public,
                })
            }
            TokenKind::Ident(_) => self.parse_func(public),
            TokenKind::Contract | TokenKind::On if public => Err(self.err(
                "`ext` applies to functions, types, and constants — not `contract`/`on`",
            )),
            TokenKind::Contract => self.parse_contract(),
            TokenKind::On => self.parse_on(),
            other => Err(self.err(format!(
                "expected a declaration, found {other:?}"
            ))),
        }
    }

    /// `use M` (whole-module import) or `use M.{ a, b }` (selective import). The
    /// `.{` after the module name signals a selection list.
    fn parse_use_import(&mut self) -> Result<Item, ParseError> {
        self.bump(); // 'use'
        let module = self.expect_ident("module name after `use`")?;
        let names = if matches!(self.peek(), TokenKind::Dot) {
            self.bump(); // '.'
            self.expect(&TokenKind::LBrace, "'{' for a selective import `use M.{ a, b }`")?;
            let mut names = Vec::new();
            while !matches!(self.peek(), TokenKind::RBrace) {
                names.push(self.expect_ident("imported member name")?);
                if matches!(self.peek(), TokenKind::Comma) {
                    self.bump();
                } else {
                    break;
                }
            }
            self.expect(&TokenKind::RBrace, "'}' to close the import selection")?;
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
        let contract = if matches!(self.peek(), TokenKind::Colon) {
            self.bump();
            Some(self.parse_type()?)
        } else {
            None
        };
        self.expect(&TokenKind::LBrace, "'{' after on target")?;
        let mut methods = Vec::new();
        while !matches!(self.peek(), TokenKind::RBrace) {
            if matches!(self.peek(), TokenKind::Eof) {
                return Err(self.err("unterminated `on` block: expected '}'"));
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
        self.expect(&TokenKind::LBrace, "'{' after contract name")?;
        let mut methods = Vec::new();
        while !matches!(self.peek(), TokenKind::RBrace) {
            if matches!(self.peek(), TokenKind::Eof) {
                return Err(self.err("unterminated contract: expected '}'"));
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
        let modifier = if matches!(self.peek(), TokenKind::Mut) {
            self.bump();
            MethodModifier::Mut
        } else if matches!(self.peek(), TokenKind::Free) {
            self.bump();
            MethodModifier::Free
        } else {
            MethodModifier::Instance
        };
        let name = self.expect_ident("method name")?;
        self.expect(&TokenKind::LParen, "'(' after method name")?;
        let params = self.parse_params()?;
        let ret = if matches!(self.peek(), TokenKind::Arrow) {
            self.bump();
            Some(self.parse_type()?)
        } else {
            None
        };
        let uses = self.parse_uses()?;
        let body = if matches!(self.peek(), TokenKind::Eq | TokenKind::LBrace) {
            Some(self.parse_body()?)
        } else if require_body {
            return Err(self.err("expected '=' or '{' for the method body"));
        } else {
            None
        };
        Ok(Method {
            name,
            modifier,
            params,
            ret,
            uses,
            body,
        })
    }

    /// A function: `name(params) -> Ret? (= expr | { block })`.
    fn parse_func(&mut self, public: bool) -> Result<Item, ParseError> {
        let name = self.expect_ident("function name")?;
        self.expect(&TokenKind::LParen, "'(' after function name")?;
        let params = self.parse_params()?;
        let ret = if matches!(self.peek(), TokenKind::Arrow) {
            self.bump();
            Some(self.parse_type()?)
        } else {
            None
        };
        let uses = self.parse_uses()?;
        let body = self.parse_body()?;
        Ok(Item::Func {
            name,
            params,
            ret,
            uses,
            body,
            public,
        })
    }

    /// An optional `uses Cap1, Cap2, …` effects clause, after the return type
    /// and before the body. Empty when absent. Each capability is a bare name.
    fn parse_uses(&mut self) -> Result<Vec<Effect>, ParseError> {
        if !matches!(self.peek(), TokenKind::Uses) {
            return Ok(Vec::new());
        }
        self.bump();
        let mut caps = Vec::new();
        loop {
            // The capability name's span is the current token's, before `expect_ident`
            // consumes it.
            let span = self.current_span();
            let name = self.expect_ident("capability name after `uses`")?;
            caps.push(Effect::new(name, span));
            if matches!(self.peek(), TokenKind::Comma) {
                self.bump();
            } else {
                break;
            }
        }
        Ok(caps)
    }

    /// A comma-separated parameter list up to and including `)`. The `(` is
    /// already consumed. Each param is `name` or `name: Type`.
    fn parse_params(&mut self) -> Result<Vec<Param>, ParseError> {
        let mut params = Vec::new();
        if !matches!(self.peek(), TokenKind::RParen) {
            loop {
                let name = self.expect_ident("parameter name")?;
                let ty = if matches!(self.peek(), TokenKind::Colon) {
                    self.bump();
                    Some(self.parse_type()?)
                } else {
                    None
                };
                params.push(Param { name, ty });
                if matches!(self.peek(), TokenKind::Comma) {
                    self.bump();
                    if matches!(self.peek(), TokenKind::RParen) {
                        break; // trailing comma
                    }
                } else {
                    break;
                }
            }
        }
        self.expect(&TokenKind::RParen, "')' after parameters")?;
        Ok(params)
    }

    /// A function/method body: `= expr` or a `{ block }`.
    fn parse_body(&mut self) -> Result<Expr, ParseError> {
        if matches!(self.peek(), TokenKind::Eq) {
            self.bump();
            self.parse_expr(0)
        } else if matches!(self.peek(), TokenKind::LBrace) {
            let start = self.cur_start();
            self.bump();
            self.parse_block(start)
        } else {
            Err(self.err("expected '=' or '{' for the function body"))
        }
    }

    /// `prod Name<generics>(fields)`.
    fn parse_prod(&mut self, public: bool) -> Result<Item, ParseError> {
        self.bump(); // 'prod'
        let name = self.expect_ident("product type name")?;
        let generics = self.parse_generics()?;
        self.expect(&TokenKind::LParen, "'(' after product name")?;
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
        self.expect(&TokenKind::Eq, "'=' in sum declaration")?;
        if matches!(self.peek(), TokenKind::Bar) {
            self.bump(); // optional leading '|'
        }
        let mut variants = vec![self.parse_variant()?];
        while matches!(self.peek(), TokenKind::Bar) {
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
        let fields = if matches!(self.peek(), TokenKind::LParen) {
            self.bump(); // '('
            self.parse_fields()?
        } else {
            Vec::new()
        };
        Ok(Variant { name, fields })
    }

    /// Optional `<T, U, …>` generic parameters.
    fn parse_generics(&mut self) -> Result<Vec<String>, ParseError> {
        if !matches!(self.peek(), TokenKind::Lt) {
            return Ok(Vec::new());
        }
        self.bump(); // '<'
        let mut params = vec![self.expect_ident("type parameter")?];
        while matches!(self.peek(), TokenKind::Comma) {
            self.bump();
            params.push(self.expect_ident("type parameter")?);
        }
        self.expect(&TokenKind::Gt, "'>' to close type parameters")?;
        Ok(params)
    }

    /// A comma-separated field list up to and including `)`. The `(` is
    /// already consumed.
    fn parse_fields(&mut self) -> Result<Vec<Field>, ParseError> {
        let mut fields = Vec::new();
        if !matches!(self.peek(), TokenKind::RParen) {
            loop {
                fields.push(self.parse_field()?);
                if matches!(self.peek(), TokenKind::Comma) {
                    self.bump();
                    if matches!(self.peek(), TokenKind::RParen) {
                        break; // trailing comma
                    }
                } else {
                    break;
                }
            }
        }
        self.expect(&TokenKind::RParen, "')' after fields")?;
        Ok(fields)
    }

    /// One field: `ext? mut? name: Type` (named) or `ext? mut? Type` (positional).
    /// `ext` marks the field exposed on an exported type (`mut` marks it mutable);
    /// both default off, and `ext` precedes `mut` (visibility outermost, as for
    /// items).
    fn parse_field(&mut self) -> Result<Field, ParseError> {
        let public = matches!(self.peek(), TokenKind::Ext);
        if public {
            self.bump();
        }
        let mutable = matches!(self.peek(), TokenKind::Mut);
        if mutable {
            self.bump();
        }
        if matches!(self.peek(), TokenKind::Ident(_)) && matches!(self.peek_at(1), TokenKind::Colon) {
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
        if matches!(self.peek(), TokenKind::Arrow) {
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
        if matches!(self.peek(), TokenKind::At) {
            self.bump();
            return Ok(Type::SelfType);
        }
        if !matches!(self.peek(), TokenKind::LParen) {
            return self.parse_type_name();
        }
        self.bump(); // '('
        if matches!(self.peek(), TokenKind::RParen) {
            self.bump();
            return Ok(Type::Tuple(Vec::new()));
        }
        let mut elems = vec![self.parse_type()?];
        while matches!(self.peek(), TokenKind::Comma) {
            self.bump();
            if matches!(self.peek(), TokenKind::RParen) {
                break; // trailing comma
            }
            elems.push(self.parse_type()?);
        }
        self.expect(&TokenKind::RParen, "')' in type")?;
        Ok(if elems.len() == 1 {
            elems.pop().expect("len checked == 1") // `(A)` is grouping, not a tuple
        } else {
            Type::Tuple(elems)
        })
    }

    fn parse_type_name(&mut self) -> Result<Type, ParseError> {
        let name = self.expect_ident("type name")?;
        let args = if matches!(self.peek(), TokenKind::Lt) {
            self.bump(); // '<'
            let mut args = vec![self.parse_type()?];
            while matches!(self.peek(), TokenKind::Comma) {
                self.bump();
                args.push(self.parse_type()?);
            }
            self.expect(&TokenKind::Gt, "'>' to close type arguments")?;
            args
        } else {
            Vec::new()
        };
        Ok(Type::Name { name, args })
    }

    /// Parse `match subject { arm* }`. The `match` keyword is already consumed;
    /// `start` is the byte offset of that `match`.
    fn parse_match(&mut self, start: usize) -> Result<Expr, ParseError> {
        if matches!(self.peek(), TokenKind::LBrace) {
            return self.parse_subjectless_match(start);
        }
        let subject = self.parse_expr(0)?;
        self.expect(&TokenKind::LBrace, "'{' after match subject")?;
        let mut arms = Vec::new();
        while !matches!(self.peek(), TokenKind::RBrace) {
            if matches!(self.peek(), TokenKind::Eof) {
                return Err(self.err("unterminated match: expected '}'"));
            }
            arms.push(self.parse_match_arm()?);
        }
        self.bump(); // '}'
        Ok(self.spanned(start, ExprKind::Match {
            subject: Box::new(subject),
            arms,
        }))
    }

    /// Parse the subjectless `match { cond => body … _ => default }` condition
    /// table and desugar it into nested `cond => then | els` conditionals
    /// (`Expr::If`) — it's the N-ary form of the binary conditional. Each arm is
    /// `condition => body`; the table must end in a `_ => …` catch-all, which
    /// becomes the innermost else. The `{` is the current token.
    fn parse_subjectless_match(&mut self, start: usize) -> Result<Expr, ParseError> {
        self.bump(); // '{'
        let mut arms = Vec::new();
        let default = loop {
            if matches!(self.peek(), TokenKind::RBrace) {
                return Err(self.err(
                    "a subjectless `match` must end in a `_ => …` catch-all",
                ));
            }
            if matches!(self.peek(), TokenKind::Eof) {
                return Err(self.err("unterminated match: expected '}'"));
            }
            if self.at_catch_all() {
                self.bump(); // '_'
                self.bump(); // '=>'
                break self.parse_expr(0)?;
            }
            // min_bp = 1 admits every binary operator but leaves the arm's `=>`
            // for us (the same trick `parse_match_arm` uses for guards).
            let cond = self.parse_expr(1)?;
            self.expect(&TokenKind::FatArrow, "'=>' in match arm")?;
            arms.push((cond, self.parse_expr(0)?));
        };
        if !matches!(self.peek(), TokenKind::RBrace) {
            return Err(self.err(
                "a `_ => …` catch-all must be the last arm of a subjectless match",
            ));
        }
        self.bump(); // '}'
        Ok(self.spanned(start, ExprKind::SubjectlessMatch {
            arms,
            default: Box::new(default),
        }))
    }

    /// Is the parser at a `_ =>` subjectless catch-all arm?
    fn at_catch_all(&self) -> bool {
        matches!(self.peek(), TokenKind::Ident(name) if name == "_")
            && matches!(self.peek_at(1), TokenKind::FatArrow)
    }

    /// Parse one arm: `pattern (if guard)? => body`. Arms are separated by
    /// maximal munch (same as block statements).
    fn parse_match_arm(&mut self) -> Result<MatchArm, ParseError> {
        let pattern = self.parse_pattern()?;
        let guard = if matches!(self.peek(), TokenKind::If) {
            self.bump();
            // min_bp = 1 admits every binary operator but skips the `=>`
            // conditional — whose `=>` is the arm separator here.
            Some(self.parse_expr(1)?)
        } else {
            None
        };
        self.expect(&TokenKind::FatArrow, "'=>' in match arm")?;
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
        if !matches!(self.peek(), TokenKind::Bar) {
            return Ok(first);
        }
        let mut alts = vec![first];
        while matches!(self.peek(), TokenKind::Bar) {
            self.bump();
            alts.push(self.parse_pattern_atom()?);
        }
        Ok(Pattern::Or(alts))
    }

    /// Parse a single (non-or) pattern.
    fn parse_pattern_atom(&mut self) -> Result<Pattern, ParseError> {
        Ok(match self.bump().clone() {
            TokenKind::Int(n) => Pattern::Int(n),
            TokenKind::Float(f) => Pattern::Float(f),
            TokenKind::Bool(b) => Pattern::Bool(b),
            TokenKind::Str(parts) => match parts.as_slice() {
                [StrPart::Lit(text)] => Pattern::Str(text.clone()),
                [] => Pattern::Str(String::new()),
                _ => {
                    return Err(self.err(
                        "string interpolation isn't allowed in a pattern — match on a plain string literal",
                    ));
                }
            },
            TokenKind::Ident(name) if name == "_" => Pattern::Wildcard,
            TokenKind::Ident(name) if starts_uppercase(&name) => {
                let args = if matches!(self.peek(), TokenKind::LParen) {
                    self.bump(); // '('
                    self.parse_pattern_list()?
                } else {
                    Vec::new()
                };
                Pattern::Constructor { name, args }
            }
            TokenKind::Ident(name) => Pattern::Binding(name),
            TokenKind::LParen => {
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
                return Err(self.err(format!(
                    "unexpected token in pattern: {other:?}"
                )));
            }
        })
    }

    /// Parse a comma-separated pattern list up to and including `)`. The `(`
    /// is already consumed.
    fn parse_pattern_list(&mut self) -> Result<Vec<Pattern>, ParseError> {
        let mut pats = Vec::new();
        if !matches!(self.peek(), TokenKind::RParen) {
            loop {
                pats.push(self.parse_pattern()?);
                if matches!(self.peek(), TokenKind::Comma) {
                    self.bump();
                    if matches!(self.peek(), TokenKind::RParen) {
                        break; // trailing comma
                    }
                } else {
                    break;
                }
            }
        }
        self.expect(&TokenKind::RParen, "')' in pattern")?;
        Ok(pats)
    }

    fn parse_atom(&mut self) -> Result<Expr, ParseError> {
        let start = self.cur_start();
        // Clone the leading token so its borrow ends before we recurse.
        Ok(match self.bump().clone() {
            TokenKind::Int(n) => self.spanned(start, ExprKind::Int(n)),
            TokenKind::Float(f) => self.spanned(start, ExprKind::Float(f)),
            TokenKind::Bool(b) => self.spanned(start, ExprKind::Bool(b)),
            TokenKind::Ident(name) => self.spanned(start, ExprKind::Var(name)),
            TokenKind::Placeholder(name) => self.spanned(start, ExprKind::Placeholder(name)),
            // `@x` is field `x` on the receiver; bare `@` is the receiver.
            TokenKind::At if matches!(self.peek(), TokenKind::Ident(_)) => {
                let name = self.expect_ident("field name after '@'")?;
                self.spanned(start, ExprKind::Field {
                    object: Box::new(self.spanned(start, ExprKind::SelfRef)),
                    name,
                })
            }
            TokenKind::At => self.spanned(start, ExprKind::SelfRef),
            TokenKind::Str(parts) => self.spanned(start, ExprKind::Str(parse_str_segments(parts)?)),
            TokenKind::LParen => {
                // `()` unit, `(e)` grouping, `(e, …)` tuple.
                if matches!(self.peek(), TokenKind::RParen) {
                    self.bump();
                    self.spanned(start, ExprKind::Tuple(Vec::new()))
                } else {
                    let first = self.parse_expr(0)?;
                    if matches!(self.peek(), TokenKind::Comma) {
                        let mut elems = vec![first];
                        while matches!(self.peek(), TokenKind::Comma) {
                            self.bump();
                            if matches!(self.peek(), TokenKind::RParen) {
                                break; // trailing comma (incl. the `(a,)` 1-tuple)
                            }
                            elems.push(self.parse_expr(0)?);
                        }
                        self.expect(&TokenKind::RParen, "')'")?;
                        self.spanned(start, ExprKind::Tuple(elems))
                    } else {
                        self.expect(&TokenKind::RParen, "')'")?;
                        first
                    }
                }
            }
            TokenKind::LBracket => self.parse_collection(start)?,
            TokenKind::LBrace => self.parse_block(start)?,
            TokenKind::Match => self.parse_match(start)?,
            TokenKind::Semicolon => return Err(self.err(NO_SEMICOLONS)),
            other => return Err(self.err(format!("unexpected token: {other:?}"))),
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::ast::{BinOp, Expr, ExprKind, Item};
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
    fn a_parse_error_renders_line_col_and_a_caret() {
        // The stray `2` starts at byte 2 → line 1, column 3.
        let err = parse("1 2").expect_err("a trailing token is a parse error");
        assert_eq!(err.render("1 2"), "1:3: expected end of input\n1 2\n  ^");
    }

    #[test]
    fn a_lexing_error_surfaces_as_a_parse_error() {
        // A stray char must not be silently dropped — both entry points surface
        // the lex error (without the wiring, `parse_program` returns `Ok([])`).
        let expr_err = parse("`").expect_err("a stray char fails expression parse");
        assert!(expr_err.message.contains("unexpected character"), "got: {}", expr_err.message);
        let prog_err = parse_program("`").expect_err("a stray char fails program parse");
        assert!(prog_err.message.contains("unexpected character"), "got: {}", prog_err.message);
    }

    #[test]
    fn a_parse_error_on_a_later_line_renders_that_lines_number() {
        // The stray `2` is the first char of line 2 → 2:1, and the rendered
        // source line is line 2 alone. Exercises the preceding-newline path.
        let err = parse("1\n2 3").expect_err("a trailing token is a parse error");
        assert_eq!(err.render("1\n2 3"), "2:1: expected end of input\n2 3\n^");
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
    fn parses_the_cross_pipe_as_a_binary_op() {
        use crate::ast::BinOp;
        assert!(matches!(p("a ~> b").kind, ExprKind::Binary { op: BinOp::CrossPipe, .. }));
    }

    #[test]
    fn the_cross_pipe_is_left_associative() {
        use crate::ast::BinOp;
        // `a ~> b ~> c` parses as `(a ~> b) ~> c`.
        let ExprKind::Binary { op: BinOp::CrossPipe, left, .. } = p("a ~> b ~> c").kind else {
            panic!("expected a cross-pipe");
        };
        assert!(matches!(left.kind, ExprKind::Binary { op: BinOp::CrossPipe, .. }));
    }

    #[test]
    fn arithmetic_binds_tighter_than_the_cross_pipe() {
        use crate::ast::BinOp;
        // `a + b ~> f` parses as `(a + b) ~> f`, same precedence as `|>`.
        let ExprKind::Binary { op: BinOp::CrossPipe, left, .. } = p("a + b ~> f").kind else {
            panic!("expected a cross-pipe");
        };
        assert!(matches!(left.kind, ExprKind::Binary { op: BinOp::Add, .. }));
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
    fn parses_a_uses_effects_clause() {
        let one = prog("emitIt() uses Telemetry = 1");
        let Item::Func { uses, .. } = &one[0] else { panic!("expected a function") };
        let names: Vec<&str> = uses.iter().map(|effect| effect.name.as_str()).collect();
        assert_eq!(names, ["Telemetry"]);

        let many = prog("act() uses Net, Tasks { 1 }");
        let Item::Func { uses, .. } = &many[0] else { panic!("expected a function") };
        let names: Vec<&str> = uses.iter().map(|effect| effect.name.as_str()).collect();
        assert_eq!(names, ["Net", "Tasks"]);

        let none = prog("pure(x) = x");
        let Item::Func { uses, .. } = &none[0] else { panic!("expected a function") };
        assert!(uses.is_empty(), "a function with no clause has no effects");
    }

    #[test]
    fn a_uses_clause_captures_the_capability_span() {
        use crate::lexer::Span;
        // `f() uses Telemetry = 1` — `Telemetry` is at bytes 9..18; its declaration
        // span is captured (not the zero default) so a refused effect can cite it.
        let items = prog("f() uses Telemetry = 1");
        let Item::Func { uses, .. } = &items[0] else { panic!("expected a function") };
        assert_eq!(uses[0].span, Span { start: 9, end: 18 });
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
    fn subjectless_match_is_a_surface_node() {
        // The parser must preserve the subjectless match form in the AST;
        // desugaring to Expr::If is the lowering pass's job, not the parser's.
        let expr = p(r#"match { n > 0 => "pos"  _ => "neg" }"#);
        assert!(
            matches!(expr.kind, ExprKind::SubjectlessMatch { .. }),
            "expected SubjectlessMatch, got {expr:?}"
        );
    }

    #[test]
    fn placeholder_is_a_surface_node() {
        // Parser must keep Placeholder; desugaring to Lambda belongs in the lowering pass.
        let expr = p("f($)");
        let ExprKind::Call { args, .. } = expr.kind else { panic!("expected Call") };
        assert!(
            matches!(args[0].value.kind, ExprKind::Placeholder(None)),
            "expected Placeholder, got {:?}", args[0].value
        );
    }

    #[test]
    fn operator_ref_is_a_surface_node() {
        // Parser must keep OperatorRef; desugaring to Lambda belongs in the lowering pass.
        let expr = p("f(+)");
        let ExprKind::Call { args, .. } = expr.kind else { panic!("expected Call") };
        assert!(
            matches!(args[0].value.kind, ExprKind::OperatorRef(BinOp::Add)),
            "expected OperatorRef(Add), got {:?}", args[0].value
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

    #[test]
    fn parse_error_carries_the_offending_span() {
        use crate::lexer::Span;
        // "let x =" — the `=` is at bytes 6..7; the parser fails when it hits
        // EOF where an expression was expected.  The error span must not be the
        // zero-width default.
        let err = parse("let x =").expect_err("should fail on incomplete let");
        assert_ne!(err.span, Span::default(), "ParseError span should not be Span::default()");
        assert!(err.span.start > 0 || err.span.end > 0, "span should point somewhere in the source");

        // A missing `=` in a function definition — span should point at the
        // unexpected token, not at byte 0.
        let err2 = parse_program("f(x) x").expect_err("should fail on missing =");
        assert_ne!(err2.span, Span::default(), "ParseError span for program error should not be Span::default()");
    }

    #[test]
    fn an_atom_carries_the_span_of_its_token() {
        use crate::lexer::Span;
        // Leading whitespace: the `x` sits at bytes 2..3.
        assert_eq!(p("  x").span, Span { start: 2, end: 3 });
        // A multi-char literal spans its whole extent.
        assert_eq!(p("42").span, Span { start: 0, end: 2 });
    }

    #[test]
    fn a_binary_span_covers_both_operands() {
        use crate::lexer::Span;
        // `a + b` — the whole expression spans byte 0 (the `a`) through byte 5
        // (past the `b`), not just the operator.
        let expr = p("a + b");
        assert_eq!(expr.span, Span { start: 0, end: 5 });
        // The operands keep their own tighter spans.
        let ExprKind::Binary { left, right, .. } = expr.kind else { panic!("expected Binary") };
        assert_eq!(left.span, Span { start: 0, end: 1 });
        assert_eq!(right.span, Span { start: 4, end: 5 });
    }

    #[test]
    fn a_call_span_runs_from_callee_through_the_closing_paren() {
        use crate::lexer::Span;
        // `f(x)` spans 0..4; the field/call chain keeps the leftmost start.
        assert_eq!(p("f(x)").span, Span { start: 0, end: 4 });
        // A postfix chain `a.b.c` spans from `a` through the last field.
        assert_eq!(p("a.b.c").span, Span { start: 0, end: 5 });
    }
}
