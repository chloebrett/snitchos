//! Lexer: source text → `Token`s.

use core::iter::Peekable;
use core::str::CharIndices;

#[allow(clippy::wildcard_imports, reason = "alloc prelude for no_std")]
use crate::prelude::*;

/// The lexer's input cursor — a peekable char stream that also tracks the byte
/// offset of the next char, so each token can carry its source [`Span`].
struct Cursor<'a> {
    inner: Peekable<CharIndices<'a>>,
    end: usize,
}

impl<'a> Cursor<'a> {
    fn new(src: &'a str) -> Self {
        Cursor { inner: src.char_indices().peekable(), end: src.len() }
    }

    /// The next char without consuming it.
    fn peek(&mut self) -> Option<char> {
        self.inner.peek().map(|&(_, c)| c)
    }

    /// The char *after* the next one, without consuming (two-char lookahead).
    fn peek2(&self) -> Option<char> {
        self.inner.clone().nth(1).map(|(_, c)| c)
    }

    /// Consume and return the next char.
    fn next(&mut self) -> Option<char> {
        self.inner.next().map(|(_, c)| c)
    }

    /// The byte offset of the next char, or end-of-input if the cursor is spent.
    fn offset(&mut self) -> usize {
        self.inner.peek().map_or(self.end, |&(i, _)| i)
    }
}

/// A piece of a string literal: literal text, or a `{expr}` interpolation
/// whose content is pre-lexed at lex time so the parser never re-lexes.
/// The raw source string is kept alongside the tokens for error messages.
#[derive(Debug, PartialEq, Clone)]
pub enum StrPart {
    Lit(String),
    /// Pre-lexed interpolation: the tokens inside `{…}` plus any lex errors,
    /// and the raw source text for diagnostic context.
    Expr(Vec<Token>, Vec<LexError>),
}

/// A byte range `[start, end)` into the source — for diagnostics.
#[derive(Debug, Default, PartialEq, Eq, Clone, Copy)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

/// A lexical token: its kind plus where it came from in the source.
#[derive(Debug, PartialEq, Clone)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

/// Compare a spanned token directly to a bare [`TokenKind`], so tests can
/// assert `lex(src) == vec![TokenKind::…]` without spelling out every span.
impl PartialEq<TokenKind> for Token {
    fn eq(&self, kind: &TokenKind) -> bool {
        &self.kind == kind
    }
}

/// A lexical error: a message plus the source [`Span`] it points at.
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct LexError {
    pub message: String,
    pub span: Span,
}

/// The result of lexing: the full token stream plus any errors. Lexing is
/// non-fatal — a malformed literal still yields a token (a zero value) and an
/// unknown character is skipped, but each is *recorded* here, so the parser can
/// surface errors rather than silently miscompiling.
pub struct Lexed {
    pub tokens: Vec<Token>,
    pub errors: Vec<LexError>,
}

/// A lexical token's kind (the "what", without the "where").
#[derive(Debug, PartialEq, Clone)]
pub enum TokenKind {
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
    Ext,
    Free,
    Use,
    Uses,
    Match,
    Handle,
    With,
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
    LArrow,    // <-
    FatArrow,  // =>
    Bar,       // |
    Pipe,      // |>
    CrossPipe, // ~>  (the cross-process pipe)
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
fn keyword(word: &str) -> Option<TokenKind> {
    Some(match word {
        "prod" => TokenKind::Prod,
        "sum" => TokenKind::Sum,
        "contract" => TokenKind::Contract,
        "on" => TokenKind::On,
        "let" => TokenKind::Let,
        "mut" => TokenKind::Mut,
        "ext" => TokenKind::Ext,
        "free" => TokenKind::Free,
        "use" => TokenKind::Use,
        "uses" => TokenKind::Uses,
        "match" => TokenKind::Match,
        "handle" => TokenKind::Handle,
        "with" => TokenKind::With,
        "and" => TokenKind::And,
        "or" => TokenKind::Or,
        "not" => TokenKind::Not,
        "if" => TokenKind::If,
        "true" => TokenKind::Bool(true),
        "false" => TokenKind::Bool(false),
        _ => return None,
    })
}

/// Tokenize Stitch source text. Each branch delegates to a kind-specific
/// helper; the helpers all consume their own characters from `chars`.
#[must_use]
pub fn lex(src: &str) -> Lexed {
    let mut tokens = Vec::new();
    let mut errors = Vec::new();
    let mut chars = Cursor::new(src);
    while let Some(c) = chars.peek() {
        let start = chars.offset();
        let kind = if c.is_whitespace() {
            chars.next();
            None
        } else if c.is_ascii_digit() {
            Some(lex_number(&mut chars, start, &mut errors))
        } else if c.is_ascii_alphabetic() || c == '_' {
            Some(lex_word(&mut chars))
        } else if c == '"' {
            Some(lex_string(&mut chars))
        } else if c == '$' {
            Some(lex_placeholder(&mut chars))
        } else if c == '/' && matches!(chars.peek2(), Some('/' | '*')) {
            skip_comment(&mut chars);
            None
        } else if let Some(kind) = lex_operator(&mut chars) {
            Some(kind)
        } else {
            // `lex_operator` consumed a non-whitespace char it didn't recognize.
            errors.push(LexError {
                message: format!("unexpected character `{c}`"),
                span: Span { start, end: chars.offset() },
            });
            None
        };
        if let Some(kind) = kind {
            let end = chars.offset();
            tokens.push(Token { kind, span: Span { start, end } });
        }
    }
    let end = src.len();
    tokens.push(Token { kind: TokenKind::Eof, span: Span { start: end, end } });
    Lexed { tokens, errors }
}

/// Read a run of `[A-Za-z0-9_]` from the cursor (the tail of a word).
fn read_word(chars: &mut Cursor<'_>) -> String {
    let mut s = String::new();
    while let Some(d) = chars.peek() {
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
fn lex_number(chars: &mut Cursor<'_>, start: usize, errors: &mut Vec<LexError>) -> TokenKind {
    let mut text = String::new();
    let mut is_float = false;
    loop {
        match chars.peek() {
            Some('_') => {
                chars.next();
            }
            Some(d) if d.is_ascii_digit() => {
                text.push(d);
                chars.next();
            }
            Some('.')
                if !is_float && matches!(chars.peek2(), Some(d) if d.is_ascii_digit()) =>
            {
                is_float = true;
                text.push('.');
                chars.next();
            }
            _ => break,
        }
    }
    let end = chars.offset();
    if is_float {
        text.parse().map_or_else(
            |_| {
                errors.push(LexError {
                    message: format!("invalid float literal `{text}`"),
                    span: Span { start, end },
                });
                TokenKind::Float(0.0)
            },
            TokenKind::Float,
        )
    } else {
        text.parse().map_or_else(
            |_| {
                errors.push(LexError {
                    message: format!("integer literal `{text}` out of range"),
                    span: Span { start, end },
                });
                TokenKind::Int(0)
            },
            TokenKind::Int,
        )
    }
}

/// Lex a word, resolving it to a keyword token or an identifier.
fn lex_word(chars: &mut Cursor<'_>) -> TokenKind {
    let word = read_word(chars);
    keyword(&word).unwrap_or(TokenKind::Ident(word))
}

/// Lex a `$` / `$name` lambda placeholder.
fn lex_placeholder(chars: &mut Cursor<'_>) -> TokenKind {
    chars.next(); // '$'
    let name = read_word(chars);
    TokenKind::Placeholder(if name.is_empty() { None } else { Some(name) })
}

/// Lex a `"…"` string literal, splitting `{expr}` interpolations into parts.
fn lex_string(chars: &mut Cursor<'_>) -> TokenKind {
    chars.next(); // opening quote
    let mut parts = Vec::new();
    let mut lit = String::new();
    loop {
        match chars.next() {
            None | Some('"') => break,
            Some('\\') => match chars.next() {
                Some('n') => lit.push('\n'),
                Some('t') => lit.push('\t'),
                Some('r') => lit.push('\r'),
                Some('e') => lit.push('\u{1b}'),
                Some('"') => lit.push('"'),
                Some('\\') => lit.push('\\'),
                Some(other) => lit.push(other),
                None => break,
            },
            // `{{` is a literal brace; a lone `{` opens an interpolation.
            Some('{') if chars.peek() == Some('{') => {
                chars.next();
                lit.push('{');
            }
            Some('{') => {
                if !lit.is_empty() {
                    parts.push(StrPart::Lit(core::mem::take(&mut lit)));
                }
                let raw = read_interpolation(chars);
                let lexed = lex(&raw);
                parts.push(StrPart::Expr(lexed.tokens, lexed.errors));
            }
            // `}}` is a literal brace; a stray `}` in text is taken literally.
            Some('}') => {
                if chars.peek() == Some('}') {
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
    TokenKind::Str(parts)
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
        while let Some(d) = chars.peek() {
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
                Some('/') if chars.peek() == Some('*') => {
                    chars.next();
                    depth += 1;
                }
                Some('*') if chars.peek() == Some('/') => {
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
fn lex_operator(chars: &mut Cursor<'_>) -> Option<TokenKind> {
    let c = chars.next()?;
    // `eat(want)` consumes a second char if it matches, for two-char operators.
    let mut eat = |want: char| {
        if chars.peek() == Some(want) {
            chars.next();
            true
        } else {
            false
        }
    };
    Some(match c {
        '-' if eat('>') => TokenKind::Arrow,
        '-' => TokenKind::Minus,
        '=' if eat('>') => TokenKind::FatArrow,
        '=' if eat('=') => TokenKind::EqEq,
        '=' => TokenKind::Eq,
        '!' if eat('=') => TokenKind::NotEq,
        '<' if eat('=') => TokenKind::Le,
        '<' if eat('-') => TokenKind::LArrow,
        '<' => TokenKind::Lt,
        '>' if eat('=') => TokenKind::Ge,
        '>' => TokenKind::Gt,
        '|' if eat('>') => TokenKind::Pipe,
        '|' => TokenKind::Bar,
        '~' if eat('>') => TokenKind::CrossPipe,
        '?' if eat('.') => TokenKind::QuestionDot,
        '?' => TokenKind::Question,
        '.' if eat('.') => {
            if eat('=') {
                TokenKind::DotDotEq
            } else {
                TokenKind::DotDot
            }
        }
        '.' => TokenKind::Dot,
        '+' => TokenKind::Plus,
        '*' => TokenKind::Star,
        '/' => TokenKind::Slash,
        '%' => TokenKind::Percent,
        '(' => TokenKind::LParen,
        ')' => TokenKind::RParen,
        '{' => TokenKind::LBrace,
        '}' => TokenKind::RBrace,
        '[' => TokenKind::LBracket,
        ']' => TokenKind::RBracket,
        ',' => TokenKind::Comma,
        ';' => TokenKind::Semicolon,
        '@' => TokenKind::At,
        ':' => TokenKind::Colon,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::{StrPart, Token, TokenKind, lex};

    /// Lex and keep only the tokens (dropping the error channel) — for the token
    /// tests that assert on token shape.
    fn toks(src: &str) -> Vec<Token> {
        lex(src).tokens
    }

    #[test]
    fn lex_reports_a_stray_char_and_an_overflowing_int() {
        use super::Span;
        // A backtick is not a Stitch token — reported, not silently dropped.
        let stray = "`";
        let errs = lex(stray).errors;
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].span, Span { start: 0, end: 1 });
        // A literal beyond i64::MAX is reported, not silently clamped to 0.
        let over = "99999999999999999999";
        assert_eq!(lex(over).errors.len(), 1);
    }

    #[test]
    fn tokens_carry_byte_offset_spans() {
        use super::{Span, TokenKind};
        // "ab + 1": ident at bytes 0..2, '+' at 3..4, int at 5..6, Eof at 6..6.
        let tokens = toks("ab + 1");
        assert_eq!(tokens[0].kind, TokenKind::Ident("ab".to_string()));
        assert_eq!(tokens[0].span, Span { start: 0, end: 2 });
        assert_eq!(tokens[1].kind, TokenKind::Plus);
        assert_eq!(tokens[1].span, Span { start: 3, end: 4 });
        assert_eq!(tokens[2].kind, TokenKind::Int(1));
        assert_eq!(tokens[2].span, Span { start: 5, end: 6 });
        assert_eq!(tokens[3].kind, TokenKind::Eof);
        assert_eq!(tokens[3].span, Span { start: 6, end: 6 });
    }

    #[test]
    fn lexes_an_integer_literal() {
        assert_eq!(toks("42"), vec![TokenKind::Int(42), TokenKind::Eof]);
    }

    #[test]
    fn ignores_underscores_in_int_literals() {
        assert_eq!(toks("1_000"), vec![TokenKind::Int(1000), TokenKind::Eof]);
    }

    #[test]
    fn lexes_a_float_literal() {
        assert_eq!(toks("2.5"), vec![TokenKind::Float(2.5), TokenKind::Eof]);
    }

    #[test]
    fn lexes_an_identifier() {
        assert_eq!(
            toks("foo_bar2"),
            vec![TokenKind::Ident("foo_bar2".to_string()), TokenKind::Eof]
        );
    }

    #[test]
    fn lexes_keywords_and_bool_literals() {
        assert_eq!(toks("let"), vec![TokenKind::Let, TokenKind::Eof]);
        assert_eq!(
            toks("true false"),
            vec![TokenKind::Bool(true), TokenKind::Bool(false), TokenKind::Eof]
        );
    }

    #[test]
    fn a_non_keyword_word_stays_an_identifier() {
        assert_eq!(
            toks("letter"),
            vec![TokenKind::Ident("letter".to_string()), TokenKind::Eof]
        );
    }

    #[test]
    fn lexes_single_char_punctuation() {
        use TokenKind::{
            At, Colon, Comma, Eof, LBrace, LBracket, LParen, Percent, Plus, RBrace, RBracket,
            RParen, Semicolon, Slash, Star,
        };
        assert_eq!(
            toks("+ * / % ( ) { } [ ] , ; @ :"),
            vec![
                Plus, Star, Slash, Percent, LParen, RParen, LBrace, RBrace, LBracket, RBracket,
                Comma, Semicolon, At, Colon, Eof,
            ]
        );
    }

    #[test]
    fn lexes_multi_char_operators() {
        use TokenKind::{
            Arrow, Bar, Eof, Eq, EqEq, FatArrow, Ge, Gt, Le, Lt, Minus, NotEq, Pipe, Question,
            QuestionDot,
        };
        assert_eq!(
            toks("- -> = == => < <= > >= != | |> ? ?."),
            vec![
                Minus, Arrow, Eq, EqEq, FatArrow, Lt, Le, Gt, Ge, NotEq, Bar, Pipe, Question,
                QuestionDot, Eof,
            ]
        );
    }

    #[test]
    fn lexes_the_dot_family() {
        use TokenKind::{Dot, DotDot, DotDotEq, Eof};
        assert_eq!(toks(". .. ..="), vec![Dot, DotDot, DotDotEq, Eof]);
    }

    #[test]
    fn lexes_the_cross_pipe() {
        use TokenKind::{CrossPipe, Eof, Pipe};
        // `~>` is its own token, distinct from the in-process `|>`.
        assert_eq!(toks("|> ~>"), vec![Pipe, CrossPipe, Eof]);
    }

    #[test]
    fn a_range_glues_to_its_operands() {
        use TokenKind::{DotDot, Eof, Ident, Int};
        assert_eq!(
            toks("0..n"),
            vec![Int(0), DotDot, Ident("n".to_string()), Eof]
        );
    }

    #[test]
    fn lexes_placeholders() {
        use TokenKind::{Eof, Placeholder};
        assert_eq!(toks("$"), vec![Placeholder(None), Eof]);
        assert_eq!(toks("$a"), vec![Placeholder(Some("a".to_string())), Eof]);
    }

    #[test]
    fn lexes_a_plain_string() {
        assert_eq!(
            toks("\"hello\""),
            vec![TokenKind::Str(vec![StrPart::Lit("hello".to_string())]), TokenKind::Eof]
        );
    }

    #[test]
    fn processes_string_escapes() {
        // source: "a\nb\"c"  → a, newline, b, quote, c
        assert_eq!(
            toks("\"a\\nb\\\"c\""),
            vec![
                TokenKind::Str(vec![StrPart::Lit("a\nb\"c".to_string())]),
                TokenKind::Eof
            ]
        );
    }

    #[test]
    fn processes_escape_and_carriage_return_escapes() {
        // `\e` is the ESC control char (0x1b) for ANSI terminal sequences; `\r` is
        // carriage return. source: "\e[H\r" → ESC, [, H, CR.
        assert_eq!(
            toks("\"\\e[H\\r\""),
            vec![
                TokenKind::Str(vec![StrPart::Lit("\u{1b}[H\r".to_string())]),
                TokenKind::Eof
            ]
        );
    }

    #[test]
    fn interp_expr_carries_pre_lexed_tokens() {
        // Interpolations must be lexed at lex time; the parser must not re-lex.
        // A `{name}` interpolation should carry a pre-lexed Ident token, not a raw String.
        let tokens = toks("\"hi {name}!\"");
        let TokenKind::Str(ref parts) = tokens[0].kind else { panic!("expected Str") };
        let StrPart::Expr(ref interp_tokens, _) = parts[1] else {
            panic!("expected Expr with tokens, got {:?}", parts[1])
        };
        assert!(
            matches!(interp_tokens[0].kind, TokenKind::Ident(_)),
            "first interp token should be Ident, got {:?}", interp_tokens[0]
        );
    }

    #[test]
    fn lexes_string_interpolation() {
        // source: "hi {name}!" → three parts: Lit + Expr + Lit
        let tokens = toks("\"hi {name}!\"");
        let TokenKind::Str(ref parts) = tokens[0].kind else { panic!("expected Str token") };
        assert_eq!(parts.len(), 3);
        assert!(matches!(&parts[0], StrPart::Lit(s) if s == "hi "));
        assert!(matches!(&parts[1], StrPart::Expr(_, _)));
        assert!(matches!(&parts[2], StrPart::Lit(s) if s == "!"));
        assert!(tokens[1] == TokenKind::Eof);
    }

    #[test]
    fn escapes_literal_braces() {
        // source: "{{x}}" → the literal text {x}
        assert_eq!(
            toks("\"{{x}}\""),
            vec![TokenKind::Str(vec![StrPart::Lit("{x}".to_string())]), TokenKind::Eof]
        );
    }

    #[test]
    fn skips_line_comments() {
        assert_eq!(toks("1 // comment\n2"), vec![TokenKind::Int(1), TokenKind::Int(2), TokenKind::Eof]);
    }

    #[test]
    fn skips_nested_block_comments() {
        assert_eq!(
            toks("1 /* a /* nested */ b */ 2"),
            vec![TokenKind::Int(1), TokenKind::Int(2), TokenKind::Eof]
        );
    }

    #[test]
    fn a_bare_slash_still_divides() {
        assert_eq!(
            toks("1 / 2"),
            vec![TokenKind::Int(1), TokenKind::Slash, TokenKind::Int(2), TokenKind::Eof]
        );
    }
}
