//! Parser: tokens → AST. A Pratt parser, grown per the §2 precedence table
//! (`plans/lang/01-grammar-and-precedence.md`). For now: a lone int literal.

use crate::ast::{BinOp, Expr};
use crate::lexer::{Token, lex};

/// Parse Stitch source into an expression.
///
/// # Panics
/// The v0 parser is happy-path only: it panics on an unexpected token.
/// Proper `Result`-based parse errors are a later increment.
#[must_use]
pub fn parse(src: &str) -> Expr {
    Parser::new(src).parse_expr(0)
}

/// Map an infix-operator token to its `BinOp`, or `None` if it isn't one.
fn infix_op(tok: &Token) -> Option<BinOp> {
    Some(match tok {
        Token::Plus => BinOp::Add,
        Token::Minus => BinOp::Sub,
        Token::Star => BinOp::Mul,
        Token::Slash => BinOp::Div,
        Token::Percent => BinOp::Rem,
        _ => return None,
    })
}

/// `(left, right)` binding powers (§2 precedence table). Loosest = lowest;
/// left < right gives left-associativity.
fn binding_power(op: BinOp) -> (u8, u8) {
    match op {
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

    /// Pratt precedence climbing: parse an expression whose operators bind at
    /// least as tightly as `min_bp`.
    fn parse_expr(&mut self, min_bp: u8) -> Expr {
        let mut left = self.parse_primary();
        while let Some(op) = infix_op(self.peek()) {
            let (l_bp, r_bp) = binding_power(op);
            if l_bp < min_bp {
                break;
            }
            self.bump(); // consume the operator
            let right = self.parse_expr(r_bp);
            left = Expr::Binary {
                op,
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        left
    }

    fn parse_primary(&mut self) -> Expr {
        // Clone the leading token so its borrow ends before we recurse.
        match self.bump().clone() {
            Token::Int(n) => Expr::Int(n),
            Token::Float(f) => Expr::Float(f),
            Token::Bool(b) => Expr::Bool(b),
            Token::Ident(name) => Expr::Var(name),
            Token::LParen => {
                let inner = self.parse_expr(0);
                match self.bump() {
                    Token::RParen => inner,
                    other => panic!("expected ')', found {other:?}"),
                }
            }
            other => panic!("unexpected token: {other:?}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::parser::parse;

    #[test]
    fn parses_an_integer_literal() {
        insta::assert_debug_snapshot!(parse("42"));
    }

    #[test]
    fn parses_a_float_literal() {
        insta::assert_debug_snapshot!(parse("3.14"));
    }

    #[test]
    fn parses_a_bool_literal() {
        insta::assert_debug_snapshot!(parse("true"));
    }

    #[test]
    fn parses_a_variable_reference() {
        insta::assert_debug_snapshot!(parse("foo"));
    }

    #[test]
    fn parses_addition() {
        insta::assert_debug_snapshot!(parse("1 + 2"));
    }

    #[test]
    fn multiplication_binds_tighter_than_addition() {
        insta::assert_debug_snapshot!(parse("1 + 2 * 3"));
    }

    #[test]
    fn parentheses_override_precedence() {
        insta::assert_debug_snapshot!(parse("(1 + 2) * 3"));
    }
}
