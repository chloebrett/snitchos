//! Lexer: source text → `Token`s.

/// A lexical token. Grows one variant per increment as the grammar lands.
#[derive(Debug, PartialEq, Eq)]
pub enum Token {
    Int(i64),
    Eof,
}

/// Tokenize Stitch source text.
#[must_use]
pub fn lex(src: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut chars = src.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_ascii_digit() {
            let mut n = 0i64;
            while let Some(d) = chars.peek().and_then(|c| c.to_digit(10)) {
                n = n * 10 + i64::from(d);
                chars.next();
            }
            tokens.push(Token::Int(n));
        } else {
            chars.next();
        }
    }
    tokens.push(Token::Eof);
    tokens
}

#[cfg(test)]
mod tests {
    use super::{Token, lex};

    #[test]
    fn lexes_an_integer_literal() {
        assert_eq!(lex("42"), vec![Token::Int(42), Token::Eof]);
    }
}
