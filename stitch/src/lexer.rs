//! Lexer: source text → `Token`s.

/// A lexical token. Grows one variant per increment as the grammar lands.
/// A piece of a string literal: literal text, or a `{expr}` interpolation
/// whose raw source the parser sub-parses later.
#[derive(Debug, PartialEq)]
pub enum StrPart {
    Lit(String),
    Expr(String),
}

#[derive(Debug, PartialEq)]
pub enum Token {
    // Literals
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(Vec<StrPart>),
    Ident(String),
    /// Lambda placeholder: `None` is bare `$` (≡ `$a`); `Some("a")` is `$a`.
    Placeholder(Option<String>),
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
    // Operators & punctuation
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Eq,        // =
    EqEq,      // ==
    NotEq,     // !=
    Lt,        // <
    Le,        // <=
    Gt,        // >
    Ge,        // >=
    Arrow,     // ->
    FatArrow,  // =>
    Bar,       // |
    Pipe,      // |>
    Question,  // ?
    QuestionDot, // ?.
    Dot,       // .
    DotDot,    // ..  (range / spread)
    DotDotEq,  // ..= (inclusive range)
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Comma,
    Semicolon,
    At,
    Colon,
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
        } else if c == '"' {
            chars.next(); // opening quote
            let mut parts = Vec::new();
            let mut lit = String::new();
            loop {
                match chars.next() {
                    None | Some('"') => break,
                    Some('\\') => match chars.next() {
                        Some('n') => lit.push('\n'),
                        Some('t') => lit.push('\t'),
                        Some('"') => lit.push('"'),
                        Some('\\') => lit.push('\\'),
                        Some(other) => lit.push(other),
                        None => break,
                    },
                    // `{{` is a literal brace; a lone `{` opens an interpolation.
                    Some('{') if chars.peek() == Some(&'{') => {
                        chars.next();
                        lit.push('{');
                    }
                    Some('{') => {
                        if !lit.is_empty() {
                            parts.push(StrPart::Lit(std::mem::take(&mut lit)));
                        }
                        let mut expr = String::new();
                        let mut depth = 1u32;
                        loop {
                            match chars.next() {
                                None => break,
                                Some('{') => {
                                    depth += 1;
                                    expr.push('{');
                                }
                                Some('}') => {
                                    depth -= 1;
                                    if depth == 0 {
                                        break;
                                    }
                                    expr.push('}');
                                }
                                Some(ch) => expr.push(ch),
                            }
                        }
                        parts.push(StrPart::Expr(expr));
                    }
                    // `}}` is a literal brace; a stray `}` in text is taken literally.
                    Some('}') => {
                        if chars.peek() == Some(&'}') {
                            chars.next();
                        }
                        lit.push('}');
                    }
                    Some(ch) => lit.push(ch),
                }
            }
            if !lit.is_empty() || parts.is_empty() {
                parts.push(StrPart::Lit(lit));
            }
            tokens.push(Token::Str(parts));
        } else if c == '/' && matches!(chars.clone().nth(1), Some('/' | '*')) {
            chars.next(); // consume '/'
            if chars.next() == Some('/') {
                // line comment: skip to end of line (newline left for whitespace)
                while let Some(&d) = chars.peek() {
                    if d == '\n' {
                        break;
                    }
                    chars.next();
                }
            } else {
                // block comment (we consumed the '*'), nestable
                let mut depth = 1u32;
                while depth > 0 {
                    match chars.next() {
                        None => break,
                        Some('/') if chars.peek() == Some(&'*') => {
                            chars.next();
                            depth += 1;
                        }
                        Some('*') if chars.peek() == Some(&'/') => {
                            chars.next();
                            depth -= 1;
                        }
                        Some(_) => {}
                    }
                }
            }
        } else if c == '$' {
            chars.next(); // consume '$'
            let mut name = String::new();
            while let Some(&d) = chars.peek() {
                if d.is_ascii_alphanumeric() || d == '_' {
                    name.push(d);
                    chars.next();
                } else {
                    break;
                }
            }
            tokens.push(Token::Placeholder(if name.is_empty() {
                None
            } else {
                Some(name)
            }));
        } else {
            chars.next(); // consume the first operator char `c`
            // `eat(next)` consumes a second char if it matches, for two-char operators.
            let mut eat = |want: char| {
                if chars.peek() == Some(&want) {
                    chars.next();
                    true
                } else {
                    false
                }
            };
            let tok = match c {
                '-' if eat('>') => Some(Token::Arrow),
                '-' => Some(Token::Minus),
                '=' if eat('>') => Some(Token::FatArrow),
                '=' if eat('=') => Some(Token::EqEq),
                '=' => Some(Token::Eq),
                '!' if eat('=') => Some(Token::NotEq),
                '<' if eat('=') => Some(Token::Le),
                '<' => Some(Token::Lt),
                '>' if eat('=') => Some(Token::Ge),
                '>' => Some(Token::Gt),
                '|' if eat('>') => Some(Token::Pipe),
                '|' => Some(Token::Bar),
                '?' if eat('.') => Some(Token::QuestionDot),
                '?' => Some(Token::Question),
                '.' if eat('.') => Some(if eat('=') {
                    Token::DotDotEq
                } else {
                    Token::DotDot
                }),
                '.' => Some(Token::Dot),
                '+' => Some(Token::Plus),
                '*' => Some(Token::Star),
                '/' => Some(Token::Slash),
                '%' => Some(Token::Percent),
                '(' => Some(Token::LParen),
                ')' => Some(Token::RParen),
                '{' => Some(Token::LBrace),
                '}' => Some(Token::RBrace),
                '[' => Some(Token::LBracket),
                ']' => Some(Token::RBracket),
                ',' => Some(Token::Comma),
                ';' => Some(Token::Semicolon),
                '@' => Some(Token::At),
                ':' => Some(Token::Colon),
                _ => None,
            };
            if let Some(t) = tok {
                tokens.push(t);
            }
        }
    }
    tokens.push(Token::Eof);
    tokens
}

#[cfg(test)]
mod tests {
    use super::{StrPart, Token, lex};

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

    #[test]
    fn lexes_single_char_punctuation() {
        use Token::{
            At, Colon, Comma, Eof, LBrace, LBracket, LParen, Percent, Plus, RBrace, RBracket,
            RParen, Semicolon, Slash, Star,
        };
        assert_eq!(
            lex("+ * / % ( ) { } [ ] , ; @ :"),
            vec![
                Plus, Star, Slash, Percent, LParen, RParen, LBrace, RBrace, LBracket, RBracket,
                Comma, Semicolon, At, Colon, Eof,
            ]
        );
    }

    #[test]
    fn lexes_multi_char_operators() {
        use Token::{
            Arrow, Bar, Eof, Eq, EqEq, FatArrow, Ge, Gt, Le, Lt, Minus, NotEq, Pipe, Question,
            QuestionDot,
        };
        assert_eq!(
            lex("- -> = == => < <= > >= != | |> ? ?."),
            vec![
                Minus, Arrow, Eq, EqEq, FatArrow, Lt, Le, Gt, Ge, NotEq, Bar, Pipe, Question,
                QuestionDot, Eof,
            ]
        );
    }

    #[test]
    fn lexes_the_dot_family() {
        use Token::{Dot, DotDot, DotDotEq, Eof};
        assert_eq!(lex(". .. ..="), vec![Dot, DotDot, DotDotEq, Eof]);
    }

    #[test]
    fn a_range_glues_to_its_operands() {
        use Token::{DotDot, Eof, Ident, Int};
        assert_eq!(
            lex("0..n"),
            vec![Int(0), DotDot, Ident("n".to_string()), Eof]
        );
    }

    #[test]
    fn lexes_placeholders() {
        use Token::{Eof, Placeholder};
        assert_eq!(lex("$"), vec![Placeholder(None), Eof]);
        assert_eq!(lex("$a"), vec![Placeholder(Some("a".to_string())), Eof]);
    }

    #[test]
    fn lexes_a_plain_string() {
        assert_eq!(
            lex("\"hello\""),
            vec![Token::Str(vec![StrPart::Lit("hello".to_string())]), Token::Eof]
        );
    }

    #[test]
    fn processes_string_escapes() {
        // source: "a\nb\"c"  → a, newline, b, quote, c
        assert_eq!(
            lex("\"a\\nb\\\"c\""),
            vec![
                Token::Str(vec![StrPart::Lit("a\nb\"c".to_string())]),
                Token::Eof
            ]
        );
    }

    #[test]
    fn lexes_string_interpolation() {
        // source: "hi {name}!"
        assert_eq!(
            lex("\"hi {name}!\""),
            vec![
                Token::Str(vec![
                    StrPart::Lit("hi ".to_string()),
                    StrPart::Expr("name".to_string()),
                    StrPart::Lit("!".to_string()),
                ]),
                Token::Eof
            ]
        );
    }

    #[test]
    fn escapes_literal_braces() {
        // source: "{{x}}" → the literal text {x}
        assert_eq!(
            lex("\"{{x}}\""),
            vec![Token::Str(vec![StrPart::Lit("{x}".to_string())]), Token::Eof]
        );
    }

    #[test]
    fn skips_line_comments() {
        assert_eq!(lex("1 // comment\n2"), vec![Token::Int(1), Token::Int(2), Token::Eof]);
    }

    #[test]
    fn skips_nested_block_comments() {
        assert_eq!(
            lex("1 /* a /* nested */ b */ 2"),
            vec![Token::Int(1), Token::Int(2), Token::Eof]
        );
    }

    #[test]
    fn a_bare_slash_still_divides() {
        assert_eq!(
            lex("1 / 2"),
            vec![Token::Int(1), Token::Slash, Token::Int(2), Token::Eof]
        );
    }
}
