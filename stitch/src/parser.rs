//! Parser: tokens → AST. A Pratt parser over the §2 precedence table
//! (`plans/lang/01-grammar-and-precedence.md`). Expression grammar so far:
//! literals, variables, unary/binary operators, grouping, and the postfix
//! layer (calls, field access, `?.`, `?`, indexing).

use std::collections::BTreeSet;

use crate::ast::{BinOp, Expr, Stmt, StrSegment, UnOp};
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

/// Parse Stitch source into an expression, or return a `ParseError`.
///
/// # Errors
/// Returns `Err` on an unexpected/missing token or trailing input.
pub fn parse(src: &str) -> Result<Expr, ParseError> {
    let mut parser = Parser::new(src);
    let expr = parser.parse_expr(0)?;
    parser.expect(&Token::Eof, "end of input")?;
    Ok(expr)
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
        Expr::Unary { operand, .. } | Expr::Try(operand) => collect_placeholders(operand, params),
        Expr::Call { callee, args } => {
            collect_placeholders(callee, params);
            for arg in args {
                collect_placeholders(arg, params);
            }
        }
        Expr::Field { object, .. } | Expr::SafeField { object, .. } => {
            collect_placeholders(object, params);
        }
        Expr::Index { object, index } => {
            collect_placeholders(object, params);
            collect_placeholders(index, params);
        }
        Expr::If { cond, then, els } => {
            collect_placeholders(cond, params);
            collect_placeholders(then, params);
            collect_placeholders(els, params);
        }
        Expr::List(items) => {
            for item in items {
                collect_placeholders(item, params);
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
        | Expr::Str(_)
        | Expr::Lambda { .. }
        | Expr::Block { .. } => {}
    }
}

/// Convert lexer string parts into AST segments, sub-parsing each `{expr}`
/// interpolation's raw source into a full expression.
fn parse_str_segments(parts: Vec<StrPart>) -> Result<Vec<StrSegment>, ParseError> {
    parts
        .into_iter()
        .map(|part| match part {
            StrPart::Lit(text) => Ok(StrSegment::Lit(text)),
            StrPart::Expr(raw) => Ok(StrSegment::Interp(Box::new(parse(&raw)?))),
        })
        .collect()
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
            let right = self.parse_expr(r_bp)?;
            left = Expr::Binary {
                op,
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        // The `cond => then | els` conditional binds looser than any binary
        // operator, so only consider it at the top level (not in operand
        // recursion): `a + b => c | d` is `(a + b) => c | d`.
        if min_bp == 0 && matches!(self.peek(), Token::FatArrow) {
            self.bump(); // =>
            let then = self.parse_expr(0)?;
            self.expect(&Token::Bar, "'|' in conditional")?;
            let els = self.parse_expr(0)?;
            left = Expr::If {
                cond: Box::new(left),
                then: Box::new(then),
                els: Box::new(els),
            };
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

    /// Prefix unary operators (`-`, `not`), binding tighter than any infix.
    fn parse_prefix(&mut self) -> Result<Expr, ParseError> {
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

    /// Parse one call argument, desugaring any `$`-placeholders it directly
    /// contains into a wrapping lambda (§3). Nested calls have already
    /// captured their own, so the placeholders left here bind to this call.
    fn parse_arg(&mut self) -> Result<Expr, ParseError> {
        let mut arg = self.parse_expr(0)?;
        let mut params = BTreeSet::new();
        collect_placeholders(&mut arg, &mut params);
        if params.is_empty() {
            Ok(arg)
        } else {
            Ok(Expr::Lambda {
                params: params.into_iter().collect(),
                body: Box::new(arg),
            })
        }
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
            } else {
                let expr = self.parse_expr(0)?;
                if matches!(self.peek(), Token::RBrace) {
                    result = Some(Box::new(expr));
                } else {
                    stmts.push(Stmt::Expr(expr));
                }
            }
        }
        self.bump(); // '}'
        Ok(Expr::Block { stmts, result })
    }

    /// Parse a `let` binding statement: `let mut? name = value`.
    fn parse_let(&mut self) -> Result<Stmt, ParseError> {
        self.bump(); // 'let'
        let mutable = matches!(self.peek(), Token::Mut);
        if mutable {
            self.bump();
        }
        let name = self.expect_ident("binding name")?;
        self.expect(&Token::Eq, "'=' in let binding")?;
        let value = self.parse_expr(0)?;
        Ok(Stmt::Let {
            name,
            mutable,
            value,
        })
    }

    fn parse_atom(&mut self) -> Result<Expr, ParseError> {
        // Clone the leading token so its borrow ends before we recurse.
        Ok(match self.bump().clone() {
            Token::Int(n) => Expr::Int(n),
            Token::Float(f) => Expr::Float(f),
            Token::Bool(b) => Expr::Bool(b),
            Token::Ident(name) => Expr::Var(name),
            Token::Placeholder(name) => Expr::Placeholder(name),
            Token::Str(parts) => Expr::Str(parse_str_segments(parts)?),
            Token::LParen => {
                let inner = self.parse_expr(0)?;
                self.expect(&Token::RParen, "')'")?;
                inner
            }
            Token::LBracket => self.parse_collection()?,
            Token::LBrace => self.parse_block()?,
            other => return Err(ParseError::new(format!("unexpected token: {other:?}"))),
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::ast::Expr;
    use crate::parser::parse;

    /// Parse, unwrapping — for the many tests whose input is valid Stitch.
    fn p(src: &str) -> Expr {
        parse(src).expect("test input should parse")
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
}
