//! babble — rung 0 of the generative ladder: a model with no weights.
//!
//! A seeded random walk over [`stitch::oracle::valid_next`]. Every emission is
//! drawn from the classes the parser says are legal, so babble cannot produce
//! syntactically invalid Stitch — only meaningless Stitch. Two hats, one walk
//! (see `docs/babble-design.md`): in batch it is the Tier-0 corpus sampler; in
//! stream it is the model behind the kvetch endpoint, which lets the whole
//! serving path be built and tested before any weights exist.
//!
//! It is also the eval floor: every trained rung is measured against babble's
//! chance-level scores.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod serve;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use stitch::oracle::{Entry, TokenClass, admits_next, all_classes, representative};

/// How many tokens a single walk may emit before it is cut off. Without the
/// depth damping that bias tables bring, a uniform walk can wander a long way
/// into nested constructs; the cap keeps generation bounded and the tests
/// fast. A walk that ends by *choosing* `Eof` is the normal case.
///
/// Cost is quadratic in this: every step re-parses the prefix, so doubling the
/// cap quadruples both generation and the membership check.
const MAX_TOKENS: usize = 200;

/// Number of token classes — the width of a weight table.
const CLASS_COUNT: usize = 58;

/// Fixed-point headroom for weight arithmetic. Weights are integers (no floats
/// in the sampler), so without a scale the pressure divisions collapse to 0/1
/// and the per-class floor dominates the draw.
const WEIGHT_SCALE: u32 = 1024;

/// Ceiling on the urge to finish. Chosen so the strongest damping
/// (`base × SCALE / pressure³`) still lands above the per-class floor: with a
/// base of 8 that is `8192 / 16³ = 2`, keeping obligations, extenders and
/// closers in a strict order instead of all bottoming out at 1.
const PRESSURE_CAP: u32 = 16;

/// Why a table cannot be used.
#[derive(Debug, PartialEq, Eq)]
pub enum TableError {
    /// No weight on `Eof`: such a walk can never choose to end, so it would
    /// run to [`MAX_TOKENS`] every time and emit truncated prefixes.
    CannotTerminate,
    /// No weight on the closing delimiters: the walk could open nesting it can
    /// never close, and so could never return to a declaration boundary.
    CannotClose,
    /// Zero softness would divide by zero when computing pressure.
    ZeroSoftness,
}

/// The sampler's policy: how likely each token class is, and how strongly the
/// walk is pushed to finish as it grows.
///
/// Data, not code — the shape-statistics pipeline (measuring arity, nesting
/// and arm counts off real Stitch) is meant to replace these numbers without
/// touching the walk. Integer-only: no floats anywhere in the sampler.
#[derive(Clone)]
pub struct Tables {
    base: [u16; CLASS_COUNT],
    /// How many tokens of progress it takes to meaningfully raise the urge to
    /// finish. Larger = longer programs.
    softness: u32,
    /// How much one level of nesting counts toward that urge, in tokens.
    depth_weight: u32,
    /// After this many tokens, end at the first point where the program is
    /// complete.
    ///
    /// Weights alone cannot guarantee termination: damping every
    /// obligation-creating class also damps the tokens that *pay* an existing
    /// debt (the `{` a pending `match` is waiting for), so a walk can enter a
    /// construct it is then discouraged from finishing. Measured: uniform and
    /// weight-damped walks both left `Eof` legal at only 2–7 of 200 steps.
    /// Asking the oracle "is the program complete here?" is the reliable
    /// stop — the weights still shape *what* gets generated, they just no
    /// longer have to shape when it ends.
    wind_down: u32,
}

/// Openers, closers, and everything else. Nesting is what makes a walk fail to
/// terminate, so the policy is expressed in terms of it.
fn is_opener(class: TokenClass) -> bool {
    matches!(class, TokenClass::LParen | TokenClass::LBrace | TokenClass::LBracket)
}

fn is_closer(class: TokenClass) -> bool {
    matches!(class, TokenClass::RParen | TokenClass::RBrace | TokenClass::RBracket)
}

/// Classes that create an *obligation*: once emitted, the program cannot end
/// until the construct they open is finished.
///
/// These — not sheer length — are what keep a walk running to the cap.
/// Measured on the uniform walk, `Eof` was legal at only 2–7 of 200 steps,
/// because `match`/`handle`/`without` each owe a block whose arms are
/// expressions that can open further constructs. Delimiter depth cannot see
/// that debt (the keyword arrives before its `{`), so they are damped as
/// openers and counted as depth in their own right.
fn is_obligation(class: TokenClass) -> bool {
    is_opener(class)
        || matches!(
            class,
            TokenClass::Match | TokenClass::Handle | TokenClass::Without | TokenClass::If
        )
}

impl Tables {
    /// Hand-tuned starting point. Every class carries equal base weight except
    /// `Eof`, which is held back so programs are not routinely empty — at a
    /// declaration boundary `Eof` competes with only a handful of openers, so
    /// an equal share would end most programs almost immediately.
    pub const DEFAULT: Self = {
        let mut base = [8u16; CLASS_COUNT];
        base[TokenClass::Eof as usize] = 2;
        Self { base, softness: 12, depth_weight: 12, wind_down: 24 }
    };

    /// Set a class's base weight (tests and future tuning).
    pub const fn set_base(&mut self, class: TokenClass, weight: u16) {
        self.base[class as usize] = weight;
    }

    /// Is this table usable?
    ///
    /// # Errors
    /// Returns the specific defect — see [`TableError`].
    pub const fn validate(&self) -> Result<(), TableError> {
        if self.softness == 0 {
            return Err(TableError::ZeroSoftness);
        }
        if self.base[TokenClass::Eof as usize] == 0 {
            return Err(TableError::CannotTerminate);
        }
        if self.base[TokenClass::RParen as usize] == 0
            || self.base[TokenClass::RBrace as usize] == 0
            || self.base[TokenClass::RBracket as usize] == 0
        {
            return Err(TableError::CannotClose);
        }
        Ok(())
    }

    /// This class's weight given how far along and how deep the walk is.
    ///
    /// `pressure` is the urge to finish: it grows with both length and
    /// nesting. Anything that *extends* the program is divided by it, while
    /// closers and `Eof` are multiplied — so a long or deeply nested walk
    /// bends back toward a boundary and stops. Without this a uniform walk
    /// wanders until the cap cuts it off, leaving a prefix that does not
    /// parse.
    fn weight(&self, class: TokenClass, emitted: u32, depth: u32) -> u32 {
        let base = u32::from(self.base[class as usize]);
        if base == 0 {
            return 0;
        }
        // Capped so the divisions keep resolving. Uncapped, `scaled / p³`
        // saturates at the floor of 1 and so does `scaled / p²` — every class
        // ends up weight 1 and the walk degenerates to the uniform one it was
        // meant to replace, precisely when it is most desperate to finish.
        let pressure =
            (1 + (emitted + depth * self.depth_weight) / self.softness).min(PRESSURE_CAP);
        let scaled = base * WEIGHT_SCALE;
        // Every class keeps a floor of 1: a class that is the *only* legal
        // continuation must stay drawable, or the walk dead-ends mid-program
        // and emits a prefix that does not parse. The scale is what makes that
        // floor negligible rather than decisive — at weight 1 apiece, the two
        // dozen operator classes would otherwise always outvote `Eof`.
        if is_closer(class) || class == TokenClass::Eof {
            scaled * pressure
        } else if is_obligation(class) {
            // Obligations pay pressure hardest: they are what keeps `Eof` off
            // the menu at all.
            (scaled / (pressure * pressure * pressure)).max(1)
        } else {
            (scaled / (pressure * pressure)).max(1)
        }
    }
}

/// Identifier stock. Deliberately mundane, lowercase, and keyword-free —
/// `no_generated_name_is_a_keyword` is the guard, because a word that lexed as
/// a keyword would make the emitted token a *different* one from the token the
/// oracle approved.
///
/// Lowercase-only is a correctness constraint, not a style choice: the oracle
/// probes `Ident` with a lowercase representative, and the parser branches on
/// an identifier's case (`starts_uppercase` separates a constructor pattern
/// from a binding). Emitting `Point` where `x` was approved would step outside
/// what was actually checked. Generating capitalised names needs the
/// payload-aware `TokenSet` refinement that is deferred with the rest of the
/// constraint story.
const WORDS: &[&str] = &[
    "name", "count", "total", "items", "value", "result", "index", "label", "price", "user",
    "node", "edge", "size", "depth", "score", "delta", "alpha", "token", "buffer", "frame",
    "span", "task", "port", "byte", "line", "page", "entry", "field", "queue", "sink",
];

/// Literal stock, kept small and legible for the same reason.
const INTS: &[i64] = &[0, 1, 2, 3, 7, 10, 42, 100, 255, 1024];
const FLOATS: &[&str] = &["0.0", "0.5", "1.5", "2.25", "3.14", "10.0"];

/// Render `class` as source text. Payload-carrying classes draw from the stock
/// above; everything else is its one fixed lexeme.
fn render(class: TokenClass, rng: &mut Rng) -> Option<String> {
    let text = match class {
        TokenClass::Ident => String::from(WORDS[rng.below(WORDS.len())]),
        TokenClass::Int => format!("{}", INTS[rng.below(INTS.len())]),
        TokenClass::Float => String::from(FLOATS[rng.below(FLOATS.len())]),
        TokenClass::Bool => String::from(if rng.below(2) == 0 { "true" } else { "false" }),
        TokenClass::Str => {
            let mut buf = String::from("\"");
            buf.push_str(WORDS[rng.below(WORDS.len())]);
            buf.push('"');
            buf
        }
        TokenClass::Placeholder => {
            String::from(if rng.below(2) == 0 { "$" } else { "$a" })
        }
        fixed => String::from(representative(fixed)?),
    };
    Some(text)
}

/// One emission: the source as it stood *before* the token, and the class
/// chosen. The trace a test replays to check the walk never stepped outside
/// what the oracle allowed.
pub struct Step {
    pub source_before: String,
    pub class: TokenClass,
}

/// A completed walk: the program, and how it got there.
pub struct Walk {
    pub source: String,
    pub steps: Vec<Step>,
}

/// xorshift64*: a seeded statistical PRNG. Sampling randomness needs quality
/// and replayability, not unpredictability — see
/// `docs/randomness-and-entropy.md`. Deliberately not a CSPRNG.
struct Rng(u64);

impl Rng {
    /// Seeded so that seed 0 is not a fixed point (xorshift's zero state is
    /// absorbing).
    const fn new(seed: u64) -> Self {
        Self(seed ^ 0x9E37_79B9_7F4A_7C15)
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// A uniform index below `n`. Modulo bias is irrelevant at these bounds
    /// (`n` is at most the token-class count).
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// Draw a class by weight from those the oracle admits after `source`, without
/// computing the whole legal set.
///
/// Draw by weight from *all* classes, ask the oracle about just that one, and
/// on rejection drop it and redraw from the rest. The result is exactly a
/// weighted draw over the legal classes, but it typically asks a handful of
/// questions instead of all 58 — and every question is a parse, which is the
/// difference between a walk costing ~1s and one costing tens of seconds.
fn pick(source: &str, rng: &mut Rng, tables: &Tables, emitted: u32, depth: u32) -> Option<TokenClass> {
    let mut candidates: Vec<(TokenClass, u32)> = all_classes()
        .iter()
        .map(|class| (*class, tables.weight(*class, emitted, depth)))
        .filter(|(_, weight)| *weight > 0)
        .collect();
    while !candidates.is_empty() {
        let total: u32 = candidates.iter().map(|(_, weight)| *weight).sum();
        let mut draw = rng.below(total as usize) as u32;
        let index = candidates
            .iter()
            .position(|(_, weight)| {
                if draw < *weight {
                    true
                } else {
                    draw -= *weight;
                    false
                }
            })
            .unwrap_or(0);
        let (class, _) = candidates[index];
        if admits_next(source, source.len(), class, Entry::Program) {
            return Some(class);
        }
        candidates.swap_remove(index);
    }
    None
}

/// Walk the grammar from empty source, recording each step.
///
/// At every position the oracle says which classes are legal; the walk picks
/// one uniformly and appends its representative lexeme. Choosing `Eof` ends
/// the program — the walk cannot end any other way except by hitting
/// [`MAX_TOKENS`] or a dead prefix (which the oracle's own contract says
/// cannot arise from a legal emission).
#[must_use]
pub fn walk(seed: u64) -> Walk {
    walk_with(seed, &Tables::DEFAULT)
}

/// [`walk`], with a chosen policy.
#[must_use]
pub fn walk_with(seed: u64, tables: &Tables) -> Walk {
    walk_from("", seed, tables, Stop::WholeProgram)
}

/// When a walk should stop.
///
/// Completeness is a *policy*, not a property of the walk. The corpus hat
/// needs whole programs — a truncated prefix cannot be validated or trained
/// on. The serving hat completes at a cursor, where the right answer is a
/// legal fragment; demanding a whole program there would mean answering
/// `greet(name) {` with the rest of the file. What both keep is that every
/// emitted token was legal, so the buffer is always left **viable**.
#[derive(Debug, Clone, Copy)]
pub enum Stop {
    /// End at the first point past `wind_down` where the program is complete,
    /// rewinding if the cap arrives mid-construct.
    WholeProgram,
    /// Emit at most this many tokens, wherever that lands.
    AfterTokens(usize),
}

/// Extend `prefix` by at most `max_tokens` legal tokens.
///
/// The serving hat: a completion, not a program.
#[must_use]
pub fn complete(prefix: &str, seed: u64, max_tokens: usize) -> String {
    walk_from(prefix, seed, &Tables::DEFAULT, Stop::AfterTokens(max_tokens)).source
}

/// The walk itself, from any starting prefix under any stopping policy.
///
/// Note for the serving hat: nesting depth restarts at zero rather than being
/// recovered from `prefix`, so the finishing pressure is measured over the
/// completion, not the whole buffer. Fine while completions are short.
#[must_use]
pub fn walk_from(prefix: &str, seed: u64, tables: &Tables, stop: Stop) -> Walk {
    let budget = match stop {
        Stop::WholeProgram => MAX_TOKENS,
        Stop::AfterTokens(n) => n.min(MAX_TOKENS),
    };
    let mut rng = Rng::new(seed);
    let mut source = String::from(prefix);
    let mut steps = Vec::new();
    let mut depth = 0u32;
    // The most recent point at which the program was already complete, as
    // (source length, step count) — the fallback if the walk runs to the cap
    // still owing a construct.
    let mut complete_at = (prefix.len(), 0usize);
    for emitted in 0..budget {
        if matches!(stop, Stop::WholeProgram)
            && admits_next(&source, source.len(), TokenClass::Eof, Entry::Program)
        {
            complete_at = (source.len(), steps.len());
            if emitted >= tables.wind_down as usize {
                steps.push(Step { source_before: source.clone(), class: TokenClass::Eof });
                return Walk { source, steps };
            }
        }
        let Some(class) = pick(&source, &mut rng, tables, emitted as u32, depth) else {
            break; // a dead prefix: unreachable from a legal emission
        };
        if is_obligation(class) {
            depth += 1;
        } else if is_closer(class) {
            depth = depth.saturating_sub(1);
        }
        steps.push(Step { source_before: source.clone(), class });
        let Some(lexeme) = render(class, &mut rng) else {
            break; // Eof: the program is complete
        };
        // The separator keeps this a walk over *tokens*: without it adjacent
        // lexemes would munch together into a different token stream than the
        // one the oracle approved.
        if !source.is_empty() {
            source.push(' ');
        }
        source.push_str(&lexeme);
    }
    if matches!(stop, Stop::AfterTokens(_)) {
        // A completion stops where its budget ran out. It is a fragment by
        // design — every token in it was legal, so the buffer is left viable.
        return Walk { source, steps };
    }
    // The cap arrived while the walk still owed a construct. Rewind to the
    // last point where the program *was* whole: a babbled *program* is always
    // a complete program, never a truncated prefix. (Rewinding to nothing
    // yields the empty program, which is valid Stitch.)
    let (len, step_count) = complete_at;
    source.truncate(len);
    steps.truncate(step_count);
    steps.push(Step { source_before: source.clone(), class: TokenClass::Eof });
    Walk { source, steps }
}

/// Generate one program from `seed`.
#[must_use]
pub fn generate(seed: u64) -> String {
    walk(seed).source
}

#[cfg(test)]
mod tests {
    use super::generate;
    use stitch::oracle::TokenClass;

    #[test]
    fn a_seed_reproduces_its_program_exactly() {
        // Replayability is the whole entropy contract: a completion recorded in
        // a span must be reconstructible from its seed, on any engine.
        assert_eq!(generate(7), generate(7));
    }

    #[test]
    fn different_seeds_diverge() {
        let programs: Vec<String> = (0..8).map(generate).collect();
        let distinct = programs.iter().collect::<alloc::collections::BTreeSet<_>>();
        assert!(distinct.len() > 1, "a seeded walk should not be constant");
    }

    #[test]
    fn a_walk_ends_by_choosing_eof_rather_than_hitting_the_cap() {
        // Termination is the bias tables' job: openers damp and closers gain
        // weight as the walk gets longer and deeper, so it works its way back
        // to a declaration boundary and stops. A walk that runs to the cap is
        // a truncated prefix, not a program — it would not even parse.
        for seed in 0..24 {
            let walked = super::walk(seed);
            assert_eq!(
                walked.steps.last().map(|s| s.class),
                Some(TokenClass::Eof),
                "seed {seed} ran to the cap: {:?}",
                walked.source
            );
        }
    }

    #[test]
    fn generated_programs_are_neither_trivial_nor_runaway() {
        // A table that terminates *too* eagerly emits `Eof` immediately and
        // produces empty programs — technically valid, useless as corpus.
        let lengths: Vec<usize> = (0..24).map(|s| super::walk(s).steps.len()).collect();
        let mean = lengths.iter().sum::<usize>() / lengths.len();
        assert!(
            (5..=super::MAX_TOKENS / 2).contains(&mean),
            "mean program length {mean} outside the useful band; lengths {lengths:?}"
        );
    }

    #[test]
    fn every_babbled_program_parses() {
        // The headline property: babble cannot emit syntactically invalid
        // Stitch, because every token it appends was approved by the parser
        // and it only stops where the parser says the program is whole.
        // A failure here means the oracle and the parser disagree somewhere.
        for seed in 0..64 {
            let source = generate(seed);
            assert!(
                stitch::parser::parse_program(&source).is_ok(),
                "seed {seed} produced unparseable output: {source:?}"
            );
        }
    }

    #[test]
    fn a_completion_extends_a_prefix_without_having_to_finish_it() {
        // The serving hat answers at a cursor, where the right answer is a
        // legal *fragment*. Demanding a whole program here would mean
        // answering `greet(name) {` with the rest of the file.
        let prefix = "greet(name) {";
        let completed = super::complete(prefix, 7, 6);
        assert!(completed.starts_with(prefix), "completion must extend the prefix");
        assert!(completed.len() > prefix.len(), "completion should add something");
        // It need not parse — but it must leave the buffer *viable*: still
        // extendable to something that does. That is the property a fragment
        // can keep, and the one an editor actually needs.
        assert!(
            !stitch::oracle::valid_next(&completed, completed.len()).is_empty(),
            "completion left the buffer dead: {completed:?}"
        );
        assert!(
            stitch::parser::parse_program(&completed).is_err(),
            "this prefix cannot be completed into a whole program in 6 tokens, \
             so the fragment should still be incomplete: {completed:?}"
        );
    }

    #[test]
    fn a_completion_respects_its_token_budget() {
        let prefix = "greet(name) {";
        let short = super::complete(prefix, 3, 2);
        let long = super::complete(prefix, 3, 12);
        assert!(short.len() < long.len(), "a larger budget should generate more");
    }

    #[test]
    fn names_and_literals_vary() {
        use alloc::collections::BTreeSet;
        use stitch::lexer::{TokenKind, lex};
        // Until terminal synthesis, every name was `x` and every number `0` —
        // the oracle answers in *classes*, and the sampler was appending the
        // same representative lexeme it probes with.
        let corpus: Vec<String> = (0..8).map(generate).collect();
        let kinds: Vec<TokenKind> =
            corpus.iter().flat_map(|s| lex(s).tokens).map(|t| t.kind).collect();
        let names: BTreeSet<&str> = kinds
            .iter()
            .filter_map(|k| match k {
                TokenKind::Ident(name) => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert!(names.len() > 3, "names should vary, got {names:?}");
        let ints: BTreeSet<i64> = kinds
            .iter()
            .filter_map(|k| match k {
                TokenKind::Int(n) => Some(*n),
                _ => None,
            })
            .collect();
        assert!(ints.len() > 1, "integers should vary, got {ints:?}");
    }

    #[test]
    fn no_generated_name_is_a_keyword() {
        use stitch::lexer::{TokenKind, lex};
        // A wordlist entry that happened to be a keyword would lex as that
        // keyword, silently making the emitted token a different one from the
        // token the oracle approved.
        for word in super::WORDS {
            let lexed = lex(word);
            assert!(
                matches!(lexed.tokens.first().map(|t| &t.kind), Some(TokenKind::Ident(_))),
                "{word:?} does not lex as an identifier"
            );
        }
    }

    #[test]
    fn a_table_that_could_never_terminate_is_rejected() {
        // Validation, not runtime hope: a table with no weight on `Eof` can
        // never end a program, and would spin to the cap every time.
        let mut tables = super::Tables::DEFAULT;
        tables.set_base(TokenClass::Eof, 0);
        assert!(tables.validate().is_err());
        assert!(super::Tables::DEFAULT.validate().is_ok());
    }

    #[test]
    fn every_emission_was_legal_where_it_landed() {
        // The sampler validated against its own oracle: re-ask at each emission
        // point and require membership. Catches a walk that appends without
        // re-consulting, or that mis-renders a class.
        for seed in 0..16 {
            for step in super::walk(seed).steps {
                assert!(
                    stitch::oracle::admits_next(
                        &step.source_before,
                        step.source_before.len(),
                        step.class,
                        stitch::oracle::Entry::Program,
                    ),
                    "seed {seed}: emitted {:?} after {:?}, which the oracle rejects",
                    step.class,
                    step.source_before,
                );
            }
        }
    }
}
