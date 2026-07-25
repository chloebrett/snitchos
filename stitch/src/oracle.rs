//! The continuation oracle: given a source prefix, which token classes may
//! legally come next?
//!
//! One pure function with four consumers (see `docs/llm-design.md`): the
//! grammar mask for constrained decoding, stim's completion affordances,
//! parser diagnostics (`expected one of …`), and live parse-state
//! highlighting. Building it once means every improvement to one lands in
//! all four.

#[allow(clippy::wildcard_imports, reason = "alloc prelude for no_std")]
use crate::prelude::*;

use crate::lexer::TokenKind;

/// A token's *class* — [`crate::lexer::TokenKind`] with the payloads stripped.
/// The oracle answers in classes because "an integer literal is legal here" is
/// a grammatical fact, while *which* integer is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TokenClass {
    // Literals
    Int,
    Float,
    Bool,
    Str,
    Ident,
    Placeholder,
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
    Without,
    And,
    Or,
    Not,
    If,
    // Operators & punctuation
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Eq,
    EqEq,
    NotEq,
    Lt,
    Le,
    Gt,
    Ge,
    Arrow,
    LArrow,
    FatArrow,
    Bar,
    Pipe,
    CrossPipe,
    Question,
    QuestionDot,
    Dot,
    DotDot,
    DotDotEq,
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

/// Every class, in discriminant order. The bitset's index↔class mapping and
/// the source of truth for [`TokenSet::to_vec`]'s ordering.
const ALL: [TokenClass; 58] = [
    TokenClass::Int,
    TokenClass::Float,
    TokenClass::Bool,
    TokenClass::Str,
    TokenClass::Ident,
    TokenClass::Placeholder,
    TokenClass::Prod,
    TokenClass::Sum,
    TokenClass::Contract,
    TokenClass::On,
    TokenClass::Let,
    TokenClass::Mut,
    TokenClass::Ext,
    TokenClass::Free,
    TokenClass::Use,
    TokenClass::Uses,
    TokenClass::Match,
    TokenClass::Handle,
    TokenClass::With,
    TokenClass::Without,
    TokenClass::And,
    TokenClass::Or,
    TokenClass::Not,
    TokenClass::If,
    TokenClass::Plus,
    TokenClass::Minus,
    TokenClass::Star,
    TokenClass::Slash,
    TokenClass::Percent,
    TokenClass::Eq,
    TokenClass::EqEq,
    TokenClass::NotEq,
    TokenClass::Lt,
    TokenClass::Le,
    TokenClass::Gt,
    TokenClass::Ge,
    TokenClass::Arrow,
    TokenClass::LArrow,
    TokenClass::FatArrow,
    TokenClass::Bar,
    TokenClass::Pipe,
    TokenClass::CrossPipe,
    TokenClass::Question,
    TokenClass::QuestionDot,
    TokenClass::Dot,
    TokenClass::DotDot,
    TokenClass::DotDotEq,
    TokenClass::LParen,
    TokenClass::RParen,
    TokenClass::LBrace,
    TokenClass::RBrace,
    TokenClass::LBracket,
    TokenClass::RBracket,
    TokenClass::Comma,
    TokenClass::Semicolon,
    TokenClass::At,
    TokenClass::Colon,
    TokenClass::Eof,
];

// The bitset is a `u64`; growing the grammar past 64 classes must not silently
// truncate the mask.
const _: () = assert!(ALL.len() <= 64);

/// A set of token classes — the oracle's answer. A `u64` bitset, because the
/// hot consumer masks a decoder's vocabulary once per emitted token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TokenSet(u64);

impl TokenSet {
    /// The empty set — nothing may follow (a dead prefix).
    pub const EMPTY: Self = Self(0);

    /// The set containing exactly `classes`.
    ///
    /// `const` so grammar-derived sets are built once at compile time; hence
    /// the index loop rather than an iterator chain.
    #[must_use]
    pub const fn of(classes: &[TokenClass]) -> Self {
        let mut bits = 0u64;
        let mut i = 0;
        while i < classes.len() {
            bits |= 1u64 << (classes[i] as u32);
            i += 1;
        }
        Self(bits)
    }

    /// Is `class` in the set?
    #[must_use]
    pub fn contains(self, class: TokenClass) -> bool {
        self.0 & (1u64 << (class as u32)) != 0
    }

    /// Is the set empty?
    #[must_use]
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// The members, in discriminant order.
    #[must_use]
    pub fn to_vec(self) -> Vec<TokenClass> {
        ALL.iter().copied().filter(|c| self.contains(*c)).collect()
    }

    /// This set plus `class`.
    #[must_use]
    pub const fn with(self, class: TokenClass) -> Self {
        Self(self.0 | (1u64 << (class as u32)))
    }
}

/// A token's class — its kind with the payload dropped.
#[must_use]
pub const fn class_of(kind: &TokenKind) -> TokenClass {
    match kind {
        TokenKind::Int(_) => TokenClass::Int,
        TokenKind::Float(_) => TokenClass::Float,
        TokenKind::Bool(_) => TokenClass::Bool,
        TokenKind::Str(_) => TokenClass::Str,
        TokenKind::Ident(_) => TokenClass::Ident,
        TokenKind::Placeholder(_) => TokenClass::Placeholder,
        TokenKind::Prod => TokenClass::Prod,
        TokenKind::Sum => TokenClass::Sum,
        TokenKind::Contract => TokenClass::Contract,
        TokenKind::On => TokenClass::On,
        TokenKind::Let => TokenClass::Let,
        TokenKind::Mut => TokenClass::Mut,
        TokenKind::Ext => TokenClass::Ext,
        TokenKind::Free => TokenClass::Free,
        TokenKind::Use => TokenClass::Use,
        TokenKind::Uses => TokenClass::Uses,
        TokenKind::Match => TokenClass::Match,
        TokenKind::Handle => TokenClass::Handle,
        TokenKind::With => TokenClass::With,
        TokenKind::Without => TokenClass::Without,
        TokenKind::And => TokenClass::And,
        TokenKind::Or => TokenClass::Or,
        TokenKind::Not => TokenClass::Not,
        TokenKind::If => TokenClass::If,
        TokenKind::Plus => TokenClass::Plus,
        TokenKind::Minus => TokenClass::Minus,
        TokenKind::Star => TokenClass::Star,
        TokenKind::Slash => TokenClass::Slash,
        TokenKind::Percent => TokenClass::Percent,
        TokenKind::Eq => TokenClass::Eq,
        TokenKind::EqEq => TokenClass::EqEq,
        TokenKind::NotEq => TokenClass::NotEq,
        TokenKind::Lt => TokenClass::Lt,
        TokenKind::Le => TokenClass::Le,
        TokenKind::Gt => TokenClass::Gt,
        TokenKind::Ge => TokenClass::Ge,
        TokenKind::Arrow => TokenClass::Arrow,
        TokenKind::LArrow => TokenClass::LArrow,
        TokenKind::FatArrow => TokenClass::FatArrow,
        TokenKind::Bar => TokenClass::Bar,
        TokenKind::Pipe => TokenClass::Pipe,
        TokenKind::CrossPipe => TokenClass::CrossPipe,
        TokenKind::Question => TokenClass::Question,
        TokenKind::QuestionDot => TokenClass::QuestionDot,
        TokenKind::Dot => TokenClass::Dot,
        TokenKind::DotDot => TokenClass::DotDot,
        TokenKind::DotDotEq => TokenClass::DotDotEq,
        TokenKind::LParen => TokenClass::LParen,
        TokenKind::RParen => TokenClass::RParen,
        TokenKind::LBrace => TokenClass::LBrace,
        TokenKind::RBrace => TokenClass::RBrace,
        TokenKind::LBracket => TokenClass::LBracket,
        TokenKind::RBracket => TokenClass::RBracket,
        TokenKind::Comma => TokenClass::Comma,
        TokenKind::Semicolon => TokenClass::Semicolon,
        TokenKind::At => TokenClass::At,
        TokenKind::Colon => TokenClass::Colon,
        TokenKind::Eof => TokenClass::Eof,
    }
}

/// A representative lexeme for each class — the token the oracle appends when
/// asking the parser "could this class come next?". `None` for [`TokenClass::Eof`],
/// which has no lexeme.
///
/// `Ident` is deliberately lowercase: where the parser branches on an
/// identifier's *payload* (`starts_uppercase` distinguishes a constructor
/// pattern from a binding), the class-level answer is the one for a binding.
/// Payload-sensitive refinement is deferred with the rest of `TokenSet`'s
/// constraint story.
const fn representative(class: TokenClass) -> Option<&'static str> {
    Some(match class {
        TokenClass::Int => "0",
        TokenClass::Float => "0.0",
        TokenClass::Bool => "true",
        TokenClass::Str => "\"\"",
        TokenClass::Ident => "x",
        TokenClass::Placeholder => "$",
        TokenClass::Prod => "prod",
        TokenClass::Sum => "sum",
        TokenClass::Contract => "contract",
        TokenClass::On => "on",
        TokenClass::Let => "let",
        TokenClass::Mut => "mut",
        TokenClass::Ext => "ext",
        TokenClass::Free => "free",
        TokenClass::Use => "use",
        TokenClass::Uses => "uses",
        TokenClass::Match => "match",
        TokenClass::Handle => "handle",
        TokenClass::With => "with",
        TokenClass::Without => "without",
        TokenClass::And => "and",
        TokenClass::Or => "or",
        TokenClass::Not => "not",
        TokenClass::If => "if",
        TokenClass::Plus => "+",
        TokenClass::Minus => "-",
        TokenClass::Star => "*",
        TokenClass::Slash => "/",
        TokenClass::Percent => "%",
        TokenClass::Eq => "=",
        TokenClass::EqEq => "==",
        TokenClass::NotEq => "!=",
        TokenClass::Lt => "<",
        TokenClass::Le => "<=",
        TokenClass::Gt => ">",
        TokenClass::Ge => ">=",
        TokenClass::Arrow => "->",
        TokenClass::LArrow => "<-",
        TokenClass::FatArrow => "=>",
        TokenClass::Bar => "|",
        TokenClass::Pipe => "|>",
        TokenClass::CrossPipe => "~>",
        TokenClass::Question => "?",
        TokenClass::QuestionDot => "?.",
        TokenClass::Dot => ".",
        TokenClass::DotDot => "..",
        TokenClass::DotDotEq => "..=",
        TokenClass::LParen => "(",
        TokenClass::RParen => ")",
        TokenClass::LBrace => "{",
        TokenClass::RBrace => "}",
        TokenClass::LBracket => "[",
        TokenClass::RBracket => "]",
        TokenClass::Comma => ",",
        TokenClass::Semicolon => ";",
        TokenClass::At => "@",
        TokenClass::Colon => ":",
        TokenClass::Eof => return None,
    })
}

/// Could a token of `class` legally follow `prefix`?
///
/// Asked of the parser itself rather than of a second copy of the grammar:
/// append the class's representative lexeme and see *where* parsing fails. An
/// error at the appended token means the parser rejected it — dead. An error
/// beyond it (or none at all) means the parser consumed it and wanted more —
/// viable, which is precisely "this prefix is still extendable". Because the
/// real parser answers, the oracle cannot drift from the grammar.
fn admits(prefix: &str, class: TokenClass) -> bool {
    let Some(lexeme) = representative(class) else {
        // `Eof`: the program may end here iff the prefix is already whole.
        return crate::parser::parse_program(prefix).is_ok();
    };
    // The separating space is what makes this a question about *tokens* rather
    // than characters — without it `le` + `t` would munch into `let`.
    let probe = format!("{prefix} {lexeme}");
    let appended_at = prefix.len() + 1;
    match crate::parser::parse_program(&probe) {
        Ok(_) => true,
        Err(err) => err.span.start > appended_at,
    }
}

/// Which token classes may legally follow `src[..pos]`?
///
/// A function of the prefix alone: everything at or after `pos` is invisible
/// (v0 contract — no consumer needs suffix-awareness yet).
///
/// Returns [`TokenSet::EMPTY`] if `pos` is not a UTF-8 boundary, or if the
/// prefix is *dead* — no token can rescue it.
#[must_use]
pub fn valid_next(src: &str, pos: usize) -> TokenSet {
    let Some(prefix) = src.get(..pos) else {
        return TokenSet::EMPTY;
    };
    ALL.iter()
        .copied()
        .filter(|class| admits(prefix, *class))
        .fold(TokenSet::EMPTY, TokenSet::with)
}

#[cfg(test)]
mod tests {
    use super::{ALL, TokenClass, class_of, representative, valid_next};

    /// The token classes that may open a top-level declaration, plus `Eof`
    /// (the empty program is a valid program). Pinned against
    /// `Parser::parse_item`'s dispatch.
    fn item_start() -> Vec<TokenClass> {
        let mut want = vec![
            TokenClass::Ext,
            TokenClass::Use,
            TokenClass::Prod,
            TokenClass::Sum,
            TokenClass::Let,
            TokenClass::Ident,
            TokenClass::Contract,
            TokenClass::On,
            TokenClass::Eof,
        ];
        want.sort_unstable();
        want
    }

    #[test]
    fn an_empty_prefix_admits_exactly_the_declaration_openers() {
        assert_eq!(valid_next("", 0).to_vec(), item_start());
    }

    #[test]
    fn after_a_complete_declaration_the_openers_are_admitted_again() {
        let src = "greet() { 1 }";
        assert_eq!(valid_next(src, src.len()).to_vec(), item_start());
    }

    /// The expected set, sorted for comparison against [`TokenSet::to_vec`].
    fn set(classes: &[TokenClass]) -> Vec<TokenClass> {
        let mut want = classes.to_vec();
        want.sort_unstable();
        want
    }

    /// What may follow this prefix, as a sorted vec.
    fn after(prefix: &str) -> Vec<TokenClass> {
        valid_next(prefix, prefix.len()).to_vec()
    }

    #[test]
    fn a_declaration_in_progress_admits_only_its_next_token() {
        // `parse_func`: the name is consumed, `(` is mandatory.
        assert_eq!(after("greet"), set(&[TokenClass::LParen]));
        // `parse_params`: a parameter name, or the empty list closes.
        assert_eq!(after("greet("), set(&[TokenClass::Ident, TokenClass::RParen]));
        // After a parameter name: `: Type`, another param, or close.
        assert_eq!(
            after("greet(a"),
            set(&[TokenClass::Colon, TokenClass::Comma, TokenClass::RParen])
        );
    }

    #[test]
    fn selective_imports_admit_their_grammar() {
        assert_eq!(after("use"), set(&[TokenClass::Ident]));
        assert_eq!(after("use M.{"), set(&[TokenClass::Ident, TokenClass::RBrace]));
        assert_eq!(
            after("use M.{ a"),
            set(&[TokenClass::Comma, TokenClass::RBrace])
        );
    }

    #[test]
    fn a_complete_item_admits_both_its_continuation_and_the_next_declaration() {
        // `use M` is already a complete import, so every declaration opener is
        // legal — but so is `.`, which continues it into `use M.{…}`.
        let mut want = item_start();
        want.push(TokenClass::Dot);
        want.sort_unstable();
        assert_eq!(after("use M"), want);
    }

    #[test]
    fn an_expression_position_admits_the_expression_openers() {
        // The expression-start set is large and grows with the grammar; assert
        // membership of representatives rather than pinning it whole.
        let start = valid_next("let x =", 7);
        for class in [
            TokenClass::Int,
            TokenClass::Str,
            TokenClass::Bool,
            TokenClass::Ident,
            TokenClass::LParen,
            TokenClass::LBracket,
            TokenClass::Minus,
            TokenClass::Not,
            TokenClass::Match,
        ] {
            assert!(start.contains(class), "{class:?} should open an expression");
        }
        // …and that it is a *mask*, not "everything": these cannot open one.
        for class in [TokenClass::RParen, TokenClass::Comma, TokenClass::Eof] {
            assert!(!start.contains(class), "{class:?} cannot open an expression");
        }
    }

    #[test]
    fn every_representative_lexes_to_the_class_it_represents() {
        use crate::lexer::lex;
        // The probe is only as honest as its lexemes: a typo here would make
        // the oracle answer a question about a *different* token.
        for class in ALL {
            let Some(lexeme) = representative(class) else {
                continue; // Eof has no lexeme
            };
            let lexed = lex(lexeme);
            assert!(lexed.errors.is_empty(), "{class:?}: {lexeme:?} does not lex");
            let kinds: Vec<_> = lexed.tokens.iter().map(|t| t.kind.clone()).collect();
            assert_eq!(
                kinds.len(),
                2,
                "{class:?}: {lexeme:?} should lex to one token + Eof, got {kinds:?}"
            );
            assert_eq!(
                class_of(&kinds[0]),
                class,
                "{class:?}: {lexeme:?} lexes as {:?}",
                kinds[0]
            );
        }
    }

    #[test]
    fn a_dead_prefix_admits_nothing() {
        // `;` is lexed but never grammatical, so nothing can rescue it.
        assert_eq!(after("greet() { 1 } ;"), Vec::new());
    }

    #[test]
    fn only_the_prefix_before_pos_is_considered() {
        // v0 contract: `valid_next` is a function of `src[..pos]`. Everything
        // at or after the cursor is invisible to it — stim will want the
        // suffix eventually, but no consumer does today, and pinning it now
        // keeps the API from drifting silently.
        assert_eq!(valid_next("prod Point { x: Int }", 0).to_vec(), item_start());
    }
}
