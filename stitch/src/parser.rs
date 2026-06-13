//! Parser: tokens → AST. A Pratt parser, grown per the §2 precedence table
//! (`plans/lang/01-grammar-and-precedence.md`). For now: a lone int literal.

use crate::ast::Expr;
use crate::lexer::{Token, lex};

/// Parse Stitch source into an expression.
///
/// # Panics
/// Temporary: panics on anything but a leading integer literal. Replaced by
/// proper `Result`-based parse errors in a later increment.
#[must_use]
pub fn parse(src: &str) -> Expr {
    let tokens = lex(src);
    let Some(&Token::Int(n)) = tokens.first() else {
        panic!("parser only handles integer literals so far");
    };
    Expr::Int(n)
}

#[cfg(test)]
mod tests {
    use crate::parser::parse;

    #[test]
    fn parses_an_integer_literal() {
        insta::assert_debug_snapshot!(parse("42"));
    }
}
