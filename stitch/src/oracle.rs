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

use crate::lexer::{LexError, Span, Token, TokenKind, lex};

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
    Test,
    Expect,
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
const ALL: [TokenClass; 60] = [
    TokenClass::Int,
    TokenClass::Float,
    TokenClass::Bool,
    TokenClass::Str,
    TokenClass::Ident,
    TokenClass::Placeholder,
    TokenClass::Prod,
    TokenClass::Sum,
    TokenClass::Contract,
    TokenClass::Test,
    TokenClass::Expect,
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

    /// Everything legal in either set.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// This set without `class`.
    #[must_use]
    pub const fn without(self, class: TokenClass) -> Self {
        Self(self.0 & !(1u64 << (class as u32)))
    }

    /// How many classes are in the set.
    #[must_use]
    pub const fn len(self) -> u32 {
        self.0.count_ones()
    }
}

/// Does this class have exactly one spelling?
///
/// The distinction a completer needs: a keyword or a delimiter can be *typed
/// for* the user, because there is only one way to write it. A class carrying a
/// payload cannot — only the user knows which identifier or which number, and
/// inserting the oracle's probe lexeme (`x`, `0`) would be inventing code
/// rather than completing it. `Bool` counts as payload-carrying: `true` and
/// `false` are two spellings, not one. `Eof` is not typed at all.
#[must_use]
pub const fn has_one_spelling(class: TokenClass) -> bool {
    !matches!(
        class,
        TokenClass::Int
            | TokenClass::Float
            | TokenClass::Bool
            | TokenClass::Str
            | TokenClass::Ident
            | TokenClass::Placeholder
            | TokenClass::Eof
    )
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
        TokenKind::Test => TokenClass::Test,
        TokenKind::Expect => TokenClass::Expect,
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
#[must_use]
pub const fn representative(class: TokenClass) -> Option<&'static str> {
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
        TokenClass::Test => "test",
        TokenClass::Expect => "expect",
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

/// How to name a class in a diagnostic. Fixed-lexeme tokens name themselves in
/// backticks; the payload-carrying classes get a description, since their
/// representative lexeme (`x`, `0`) would read as a literal demand.
#[must_use]
pub fn describe(class: TokenClass) -> String {
    match class {
        TokenClass::Int => "an integer".to_string(),
        TokenClass::Float => "a float".to_string(),
        TokenClass::Bool => "a boolean".to_string(),
        TokenClass::Str => "a string".to_string(),
        TokenClass::Ident => "a name".to_string(),
        TokenClass::Placeholder => "a placeholder".to_string(),
        TokenClass::Eof => "end of input".to_string(),
        fixed => match representative(fixed) {
            Some(lexeme) => format!("`{lexeme}`"),
            None => String::new(),
        },
    }
}

/// Which grammar the prefix is being read as. The same text has different
/// continuations under each: after `1 +` an operand is legal as an
/// *expression*, while as a *program* the prefix is already dead (an integer
/// cannot open a declaration).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Entry {
    /// A sequence of top-level declarations — a `.st` file, and what babble
    /// and stim generate.
    #[default]
    Program,
    /// A single expression — a REPL line, an interpolation body.
    Expr,
}

impl Entry {
    /// Does `src` parse whole under this entry?
    fn accepts(self, src: &str) -> bool {
        match self {
            Self::Program => crate::parser::parse_program(src).is_ok(),
            Self::Expr => crate::parser::parse(src).is_ok(),
        }
    }

    /// Where does `src` stop parsing under this entry? `None` if it parses
    /// whole.
    fn stops_at(self, src: &str) -> Option<usize> {
        let err = match self {
            Self::Program => crate::parser::parse_program(src).err(),
            Self::Expr => crate::parser::parse(src).err(),
        };
        err.map(|e| e.span.start)
    }

    /// [`Self::stops_at`], from tokens already lexed.
    fn stops_at_tokens(self, tokens: Vec<Token>, lex_errors: Vec<LexError>) -> Option<usize> {
        let err = match self {
            Self::Program => crate::parser::parse_program_tokens(tokens, lex_errors).err(),
            Self::Expr => crate::parser::parse_expr_tokens(tokens, lex_errors).err(),
        };
        err.map(|e| e.span.start)
    }
}

/// Could a token of `class` legally follow `prefix`?
///
/// Asked of the parser itself rather than of a second copy of the grammar:
/// append the class's representative lexeme and see *where* parsing fails. An
/// error at the appended token means the parser rejected it — dead. An error
/// beyond it (or none at all) means the parser consumed it and wanted more —
/// viable, which is precisely "this prefix is still extendable". Because the
/// real parser answers, the oracle cannot drift from the grammar.
fn admits(prefix: &str, class: TokenClass, entry: Entry) -> bool {
    let Some(lexeme) = representative(class) else {
        // `Eof`: input may end here iff the prefix is already whole.
        return entry.accepts(prefix);
    };
    // The separating space is what makes this a question about *tokens* rather
    // than characters — without it `le` + `t` would munch into `let`.
    let probe = format!("{prefix} {lexeme}");
    let appended_at = prefix.len() + 1;
    match entry.stops_at(&probe) {
        None => true,
        Some(stop) => stop > appended_at,
    }
}

/// The lexed prefix, reused across every probe of one query.
///
/// Re-lexing the prefix per class was the oracle's dominant cost: 58 classes ×
/// (a formatted source string + a full re-lex) per entry, which is invisible on
/// the host and exhausts a 16 MiB process heap on the metal. Lexing once and
/// appending a candidate token leaves only the parse.
struct Probes {
    /// The prefix's tokens, trailing `Eof` removed so a candidate can be pushed.
    base: Vec<Token>,
    lex_errors: Vec<LexError>,
    /// Where an appended token starts — the viability rule's pivot.
    appended_at: usize,
}

impl Probes {
    fn new(prefix: &str) -> Self {
        let lexed = lex(prefix);
        let mut base = lexed.tokens;
        base.pop(); // the trailing `Eof`
        // The separator is notional now: tokens are appended directly, so the
        // maximal-munch hazard that made a literal space necessary cannot
        // arise. The offset is kept so the "did parsing stop at the appended
        // token?" comparison reads the same as before.
        Self { base, lex_errors: lexed.errors, appended_at: prefix.len() + 1 }
    }

    /// The prefix's tokens plus `class`'s representative and `Eof`.
    fn with(&self, class: TokenClass) -> Option<Vec<Token>> {
        let kind = representative_kind(class)?;
        let lexeme_len = representative(class)?.len();
        let mut tokens = Vec::with_capacity(self.base.len() + 2);
        tokens.extend_from_slice(&self.base);
        let end = self.appended_at + lexeme_len;
        tokens.push(Token { kind, span: Span { start: self.appended_at, end } });
        tokens.push(Token { kind: TokenKind::Eof, span: Span { start: end, end } });
        Some(tokens)
    }

    fn admits(&self, class: TokenClass, entry: Entry) -> bool {
        let Some(tokens) = self.with(class) else {
            return false; // `Eof` is answered by the caller
        };
        match entry.stops_at_tokens(tokens, self.lex_errors.clone()) {
            None => true,
            Some(stop) => stop > self.appended_at,
        }
    }
}

/// A class's representative as a *token*, skipping the lexer.
///
/// Paired with [`representative`] — `every_representative_lexes_to_the_class_it_represents`
/// pins that the two agree, so this table cannot drift into probing a different
/// token than the one the sampler will later emit.
fn representative_kind(class: TokenClass) -> Option<TokenKind> {
    Some(match class {
        TokenClass::Int => TokenKind::Int(0),
        TokenClass::Float => TokenKind::Float(0.0),
        TokenClass::Bool => TokenKind::Bool(true),
        TokenClass::Str => TokenKind::Str(Vec::new()),
        TokenClass::Ident => TokenKind::Ident(String::from("x")),
        TokenClass::Placeholder => TokenKind::Placeholder(None),
        TokenClass::Eof => return None,
        fixed => {
            // Every remaining class has one fixed spelling, so its lexeme lexes
            // to exactly one token — reuse the lexer rather than restate 50
            // mappings that could drift from `representative`.
            let mut lexed = lex(representative(fixed)?);
            lexed.tokens.first_mut().map(|t| core::mem::replace(&mut t.kind, TokenKind::Eof))?
        }
    })
}

/// Which token classes may legally follow `src[..pos]`, read as a program?
///
/// A function of the prefix alone: everything at or after `pos` is invisible
/// (v0 contract — no consumer needs suffix-awareness yet).
///
/// Returns [`TokenSet::EMPTY`] if `pos` is not a UTF-8 boundary, or if the
/// prefix is *dead* — no token can rescue it.
#[must_use]
pub fn valid_next(src: &str, pos: usize) -> TokenSet {
    valid_next_in(src, pos, Entry::Program)
}

/// Every token class, in discriminant order — the candidate list a sampler
/// walks.
#[must_use]
pub const fn all_classes() -> &'static [TokenClass] {
    &ALL
}

/// May a token of `class` legally follow `src[..pos]`?
///
/// The single-class question. [`valid_next`] answers it for all 58 classes,
/// which is what a decoder mask or a diagnostic needs — but a *sampler* needs
/// only one viable class, and asking one at a time until it finds one is an
/// order of magnitude cheaper (each query is a parse).
#[must_use]
pub fn admits_next(src: &str, pos: usize, class: TokenClass, entry: Entry) -> bool {
    src.get(..pos)
        .is_some_and(|prefix| admits(prefix, class, entry))
}

/// [`valid_next`], for a chosen [`Entry`] grammar.
#[must_use]
pub fn valid_next_in(src: &str, pos: usize, entry: Entry) -> TokenSet {
    let Some(prefix) = src.get(..pos) else {
        return TokenSet::EMPTY;
    };
    // One lex for the whole query, not one per class.
    let probes = Probes::new(prefix);
    ALL.iter()
        .copied()
        .filter(|class| {
            if *class == TokenClass::Eof {
                entry.accepts(prefix)
            } else {
                probes.admits(*class, entry)
            }
        })
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
            TokenClass::Test,
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
    fn the_single_class_query_agrees_with_the_full_set() {
        use super::{Entry, admits_next, all_classes};
        // Samplers ask `admits_next` one class at a time (cheap); masks and
        // diagnostics ask `valid_next` for all of them. They must be the same
        // question — if they drift, babble emits tokens the mask forbids.
        for prefix in ["", "greet", "greet(", "use M.{", "let x =", "greet() { 1 }"] {
            let full = valid_next(prefix, prefix.len());
            for class in all_classes() {
                assert_eq!(
                    full.contains(*class),
                    admits_next(prefix, prefix.len(), *class, Entry::Program),
                    "{prefix:?} / {class:?}"
                );
            }
        }
    }

    #[test]
    fn a_dead_prefix_admits_nothing() {
        // `;` is lexed but never grammatical, so nothing can rescue it.
        assert_eq!(after("greet() { 1 } ;"), Vec::new());
    }

    #[test]
    fn the_empty_map_literal_is_reachable_one_token_at_a_time() {
        // `[:]` is legal Stitch, so a decoder constrained by this oracle has to
        // be able to *get* there — and it can only ever append one token. If
        // `Colon` is not admitted after `[`, the empty map is unreachable under
        // the mask: babble can never generate one and no masked model can emit
        // one, however much training data contains it.
        //
        // Found by scoring real Stitch: `plans/lang/samples.st` writes
        // `fold([:], …)`, and the eval harness reported the oracle rejecting a
        // token a human actually wrote.
        let prefix = "let m = [";
        assert!(
            valid_next(prefix, prefix.len()).contains(TokenClass::Colon),
            "`:` must be admitted after `[`, or `[:]` cannot be reached"
        );
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
