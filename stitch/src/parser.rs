//! Parser: tokens → AST. A Pratt parser, grown per the §2 precedence table
//! (`plans/lang/01-grammar-and-precedence.md`). For now: a lone int literal.

use crate::ast::Expr;
use crate::lexer::{Token, lex};

/// Parse Stitch source into an expression.
///
/// # Panics
/// The v0 parser is happy-path only: it panics on an unexpected token.
/// Proper `Result`-based parse errors are a later increment.
#[must_use]
pub fn parse(src: &str) -> Expr {
    Parser::new(src).parse_expr()
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

    /// Return the current token and advance past it; stops at `Eof`.
    fn bump(&mut self) -> &Token {
        let i = self.pos;
        if !matches!(self.tokens[i], Token::Eof) {
            self.pos += 1;
        }
        &self.tokens[i]
    }

    fn parse_expr(&mut self) -> Expr {
        self.parse_primary()
    }

    fn parse_primary(&mut self) -> Expr {
        match self.bump() {
            Token::Int(n) => Expr::Int(*n),
            Token::Float(f) => Expr::Float(*f),
            Token::Bool(b) => Expr::Bool(*b),
            Token::Ident(name) => Expr::Var(name.clone()),
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
}
