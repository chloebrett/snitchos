//! Lexer: source text → `Token`s.

use std::iter::Peekable;
use std::str::Chars;

/// The lexer's input cursor — a peekable stream of source chars.
type Cursor<'a> = Peekable<Chars<'a>>;

/// A piece of a string literal: literal text, or a `{expr}` interpolation
/// whose raw source the parser sub-parses later.
#[derive(Debug, PartialEq, Clone)]
pub enum StrPart {
    Lit(String),
    Expr(String),
}

/// A lexical token.
#[derive(Debug, PartialEq, Clone)]
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
    If, // reserved, but only valid as a match-arm guard
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
        "if" => Token::If,
        "true" => Token::Bool(true),
        "false" => Token::Bool(false),
        _ => return None,
    })
}

/// Tokenize Stitch source text. Each branch delegates to a kind-specific
/// helper; the helpers all consume their own characters from `chars`.
#[must_use]
pub fn lex(src: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut chars = src.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_ascii_digit() {
            tokens.push(lex_number(&mut chars));
        } else if c.is_ascii_alphabetic() || c == '_' {
            tokens.push(lex_word(&mut chars));
        } else if c == '"' {
            tokens.push(lex_string(&mut chars));
        } else if c == '$' {
            tokens.push(lex_placeholder(&mut chars));
        } else if c == '/' && matches!(chars.clone().nth(1), Some('/' | '*')) {
            skip_comment(&mut chars);
        } else if let Some(tok) = lex_operator(&mut chars) {
            tokens.push(tok);
        }
        // else: `lex_operator` consumed an unrecognized char and returned
        // `None` — skip it (lenient; lexer errors are future work).
    }
    tokens.push(Token::Eof);
    tokens
}

/// Read a run of `[A-Za-z0-9_]` from the cursor (the tail of a word).
fn read_word(chars: &mut Cursor<'_>) -> String {
    let mut s = String::new();
    while let Some(&d) = chars.peek() {
        if d.is_ascii_alphanumeric() || d == '_' {
            s.push(d);
            chars.next();
        } else {
            break;
        }
    }
    s
}

/// Lex an int or float literal. `_` separators are ignored; a `.` only
/// starts a fraction when a digit follows, so `0..n` leaves the dots alone.
fn lex_number(chars: &mut Cursor<'_>) -> Token {
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
            Some('.')
                if !is_float && matches!(chars.clone().nth(1), Some(d) if d.is_ascii_digit()) =>
            {
                is_float = true;
                text.push('.');
                chars.next();
            }
            _ => break,
        }
    }
    if is_float {
        Token::Float(text.parse().unwrap_or(0.0))
    } else {
        Token::Int(text.parse().unwrap_or(0))
    }
}

/// Lex a word, resolving it to a keyword token or an identifier.
fn lex_word(chars: &mut Cursor<'_>) -> Token {
    let word = read_word(chars);
    keyword(&word).unwrap_or(Token::Ident(word))
}

/// Lex a `$` / `$name` lambda placeholder.
fn lex_placeholder(chars: &mut Cursor<'_>) -> Token {
    chars.next(); // '$'
    let name = read_word(chars);
    Token::Placeholder(if name.is_empty() { None } else { Some(name) })
}

/// Lex a `"…"` string literal, splitting `{expr}` interpolations into parts.
fn lex_string(chars: &mut Cursor<'_>) -> Token {
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
                parts.push(StrPart::Expr(read_interpolation(chars)));
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
    Token::Str(parts)
}

/// Capture the raw source inside a `{…}` interpolation up to the matching
/// `}`, honouring nested braces. The opening `{` is already consumed.
fn read_interpolation(chars: &mut Cursor<'_>) -> String {
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
    expr
}

/// Skip a `//` line comment or a nestable `/* */` block comment.
/// The leading `/` is still on the cursor.
fn skip_comment(chars: &mut Cursor<'_>) {
    chars.next(); // '/'
    if chars.next() == Some('/') {
        while let Some(&d) = chars.peek() {
            if d == '\n' {
                break;
            }
            chars.next();
        }
    } else {
        // block comment (the '*' is consumed), nestable
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
}

/// Lex an operator or punctuation token. Consumes the leading char; returns
/// `None` (having consumed it) for an unrecognized char.
fn lex_operator(chars: &mut Cursor<'_>) -> Option<Token> {
    let c = chars.next()?;
    // `eat(want)` consumes a second char if it matches, for two-char operators.
    let mut eat = |want: char| {
        if chars.peek() == Some(&want) {
            chars.next();
            true
        } else {
            false
        }
    };
    Some(match c {
        '-' if eat('>') => Token::Arrow,
        '-' => Token::Minus,
        '=' if eat('>') => Token::FatArrow,
        '=' if eat('=') => Token::EqEq,
        '=' => Token::Eq,
        '!' if eat('=') => Token::NotEq,
        '<' if eat('=') => Token::Le,
        '<' => Token::Lt,
        '>' if eat('=') => Token::Ge,
        '>' => Token::Gt,
        '|' if eat('>') => Token::Pipe,
        '|' => Token::Bar,
        '?' if eat('.') => Token::QuestionDot,
        '?' => Token::Question,
        '.' if eat('.') => {
            if eat('=') {
                Token::DotDotEq
            } else {
                Token::DotDot
            }
        }
        '.' => Token::Dot,
        '+' => Token::Plus,
        '*' => Token::Star,
        '/' => Token::Slash,
        '%' => Token::Percent,
        '(' => Token::LParen,
        ')' => Token::RParen,
        '{' => Token::LBrace,
        '}' => Token::RBrace,
        '[' => Token::LBracket,
        ']' => Token::RBracket,
        ',' => Token::Comma,
        ';' => Token::Semicolon,
        '@' => Token::At,
        ':' => Token::Colon,
        _ => return None,
    })
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
        assert_eq!(lex("2.5"), vec![Token::Float(2.5), Token::Eof]);
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
