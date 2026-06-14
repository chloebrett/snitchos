//! Parser: tokens → AST. A Pratt parser over the §2 precedence table
//! (`plans/lang/01-grammar-and-precedence.md`). Expression grammar so far:
//! literals, variables, unary/binary operators, grouping, and the postfix
//! layer (calls, field access, `?.`, `?`, indexing).

use crate::ast::{BinOp, Expr, UnOp};
use crate::lexer::{Token, lex};

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
        Ok(left)
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
                args.push(self.parse_expr(0)?);
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

    fn parse_atom(&mut self) -> Result<Expr, ParseError> {
        // Clone the leading token so its borrow ends before we recurse.
        Ok(match self.bump().clone() {
            Token::Int(n) => Expr::Int(n),
            Token::Float(f) => Expr::Float(f),
            Token::Bool(b) => Expr::Bool(b),
            Token::Ident(name) => Expr::Var(name),
            Token::LParen => {
                let inner = self.parse_expr(0)?;
                self.expect(&Token::RParen, "')'")?;
                inner
            }
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
}
