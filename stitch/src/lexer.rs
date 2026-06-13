//! Lexer: source text → `Token`s.

/// A lexical token. Grows one variant per increment as the grammar lands.
#[derive(Debug, PartialEq)]
pub enum Token {
    // Literals
    Int(i64),
    Float(f64),
    Bool(bool),
    Ident(String),
    // Keywords
    Prod,
    Sum,
    Contract,
    On,
    Let,
    Mut,
    Free,
    Use,
    Uses,
    Match,
    And,
    Or,
    Not,
    Eof,
}

/// Map a word to its keyword token, or `None` if it's a plain identifier.
fn keyword(word: &str) -> Option<Token> {
    Some(match word {
        "prod" => Token::Prod,
        "sum" => Token::Sum,
        "contract" => Token::Contract,
        "on" => Token::On,
        "let" => Token::Let,
        "mut" => Token::Mut,
        "free" => Token::Free,
        "use" => Token::Use,
        "uses" => Token::Uses,
        "match" => Token::Match,
        "and" => Token::And,
        "or" => Token::Or,
        "not" => Token::Not,
        "true" => Token::Bool(true),
        "false" => Token::Bool(false),
        _ => return None,
    })
}

/// Tokenize Stitch source text.
#[must_use]
pub fn lex(src: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut chars = src.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_ascii_digit() {
            let mut text = String::new();
            let mut is_float = false;
            loop {
                match chars.peek().copied() {
                    Some('_') => {
                        chars.next();
                    }
                    Some(d) if d.is_ascii_digit() => {
                        text.push(d);
                        chars.next();
                    }
                    // `.` starts a fraction only when a digit follows — so `0..n`
                    // (range) leaves the dots for the operator lexer to handle.
                    Some('.')
                        if !is_float
                            && matches!(chars.clone().nth(1), Some(d) if d.is_ascii_digit()) =>
                    {
                        is_float = true;
                        text.push('.');
                        chars.next();
                    }
                    _ => break,
                }
            }
            if is_float {
                tokens.push(Token::Float(text.parse().unwrap_or(0.0)));
            } else {
                tokens.push(Token::Int(text.parse().unwrap_or(0)));
            }
        } else if c.is_ascii_alphabetic() || c == '_' {
            let mut text = String::new();
            while let Some(&d) = chars.peek() {
                if d.is_ascii_alphanumeric() || d == '_' {
                    text.push(d);
                    chars.next();
                } else {
                    break;
                }
            }
            tokens.push(keyword(&text).unwrap_or(Token::Ident(text)));
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

    #[test]
    fn ignores_underscores_in_int_literals() {
        assert_eq!(lex("1_000"), vec![Token::Int(1000), Token::Eof]);
    }

    #[test]
    fn lexes_a_float_literal() {
        assert_eq!(lex("3.14"), vec![Token::Float(3.14), Token::Eof]);
    }

    #[test]
    fn lexes_an_identifier() {
        assert_eq!(
            lex("foo_bar2"),
            vec![Token::Ident("foo_bar2".to_string()), Token::Eof]
        );
    }

    #[test]
    fn lexes_keywords_and_bool_literals() {
        assert_eq!(lex("let"), vec![Token::Let, Token::Eof]);
        assert_eq!(
            lex("true false"),
            vec![Token::Bool(true), Token::Bool(false), Token::Eof]
        );
    }

    #[test]
    fn a_non_keyword_word_stays_an_identifier() {
        assert_eq!(
            lex("letter"),
            vec![Token::Ident("letter".to_string()), Token::Eof]
        );
    }
}
