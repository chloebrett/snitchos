//! cram-eval — how every rung of the ladder is measured.
//!
//! One scoring path for all rungs, from babble (no weights at all) upward. That
//! is the point of the [`Predictor`] trait: babble's floor row and a trained
//! rung's row are produced by the same code, so they cannot become incomparable
//! through drift. "babble is rung 0 of the same ladder" stops being a claim in a
//! doc and becomes a compiled one.
//!
//! **The gate metric is held-out masked NLL** (`plans/legacy/drivel.md`). At each
//! position of a held-out program the oracle names the legal token classes; a
//! rung is a distribution over exactly that set; the score is the mean negative
//! log-likelihood of the class a human actually wrote. Exact, apples-to-apples,
//! no sampling, and stable on the few thousand held-out tokens we have.
//!
//! `unconstrained-parse%` is deliberately *not* a babble comparison — babble
//! scores 100% by construction. See [`Report`].

pub mod corpus;
pub mod generate;

use stitch::oracle::{Entry, TokenClass, TokenSet, class_of, valid_next_in};

/// Where in a program a decision is being made.
///
/// `emitted` and `depth` are carried rather than recomputed because a rung that
/// wants them (babble does — its tables damp by both) would otherwise re-lex the
/// prefix at every position, which is quadratic in program length and would
/// dominate the whole measurement on a file the size of `stim.st`. A rung that
/// does not want them ignores them.
#[derive(Debug, Clone, Copy)]
pub struct Context<'a> {
    /// The program up to this decision. Everything after is invisible — the
    /// same v0 contract the oracle keeps.
    pub prefix: &'a str,
    /// Tokens before this decision.
    pub emitted: u32,
    /// Unclosed nesting, by babble's rule: obligations open, closers close.
    pub depth: u32,
}

/// One rung's answer to "what comes next here?", as a distribution over the
/// classes the oracle admits.
///
/// The legal set is passed in rather than computed per rung: it costs one parse
/// per class to derive, and — more importantly — handing both rungs the *same*
/// set is what makes their scores comparable at all.
pub trait Predictor {
    /// The name this rung reports under.
    fn name(&self) -> &'static str;

    /// Weights over `legal`, in any positive scale. The harness normalizes, so
    /// an implementation may return logits-turned-weights, integer bias-table
    /// weights, or plain counts.
    ///
    /// Every legal class must receive positive weight. A zero is an infinite
    /// NLL, and one such position would swamp the mean of every other — so a
    /// rung that wants to say "never" must say "vanishingly rarely" instead.
    fn weights(&self, at: Context<'_>, legal: TokenSet) -> Vec<(TokenClass, f64)>;
}

/// Uniform over whatever the oracle admits — the control.
///
/// The harness exists to detect signal; this is the predictor with none. If a
/// tuned rung cannot beat it, either the rung has learned nothing or the
/// harness cannot see what it learned, and those are worth telling apart before
/// a real model is on the line.
pub struct Uniform;

impl Predictor for Uniform {
    fn name(&self) -> &'static str {
        "uniform"
    }

    fn weights(&self, _at: Context<'_>, legal: TokenSet) -> Vec<(TokenClass, f64)> {
        legal.to_vec().into_iter().map(|class| (class, 1.0)).collect()
    }
}

/// babble scored through its bias tables — rung 0's row.
///
/// Reads [`babble::distribution`], which is pinned to the sampler `babble::pick`
/// actually runs. Without that pin this would be the score of a model nobody
/// uses.
pub struct Babble {
    tables: babble::Tables,
}

impl Default for Babble {
    fn default() -> Self {
        Self { tables: babble::Tables::DEFAULT }
    }
}

impl Predictor for Babble {
    fn name(&self) -> &'static str {
        "babble"
    }

    fn weights(&self, at: Context<'_>, legal: TokenSet) -> Vec<(TokenClass, f64)> {
        // babble's walk damps by how far along and how deep it is. Scoring a
        // program babble did not walk, those come from the context the harness
        // tracked — and from `babble::distribution`, not a second copy of the
        // weight rule, so the floor row is the score of the sampler that runs.
        babble::distribution(&self.tables, legal, at.emitted, at.depth)
            .into_iter()
            .map(|(class, weight)| (class, f64::from(weight)))
            .collect()
    }
}

/// One scored decision.
#[derive(Debug, Clone)]
pub struct Decision {
    /// Index into the programs `score` was given.
    ///
    /// Without it, `position` is a byte offset into an unnamed program, and a
    /// diagnostic that resolves it against the wrong source prints a confidently
    /// wrong line — which the first version of this did.
    pub program: usize,
    /// Byte offset in the program where the class was to be chosen.
    pub position: usize,
    /// Tokens before this decision, within its program.
    ///
    /// Carried so a report can separate "this rung is bad at Stitch" from "this
    /// rung is being scored outside the regime it operates in". babble stops
    /// generating by ~50 tokens; scoring it at token 8,000 of `stim.st` asks it
    /// a question it was never built to answer, and without this field that
    /// shows up only as a bad number with no explanation.
    pub emitted: u32,
    /// What the human actually wrote there.
    pub actual: TokenClass,
    /// How many classes the oracle admitted.
    pub legal_count: u32,
    /// `-ln p(actual)`.
    pub nll: f64,
}

impl Decision {
    /// A position with one legal class is *forced*: every rung scores it 0, so
    /// it carries no signal about any of them.
    #[must_use]
    pub const fn is_forced(&self) -> bool {
        self.legal_count == 1
    }
}

/// What scoring a rung produced.
///
/// Every rate is reported beside its denominator. A bare percentage once sent
/// someone hunting through a backward pass that was correct all along.
#[derive(Debug, Clone)]
pub struct Report {
    pub rung: String,
    /// Mean `-ln p` over every decision, forced ones included. The headline.
    pub masked_nll: f64,
    /// Mean `-ln p` over decisions with a real choice. Forced positions score 0
    /// for every rung and so only dilute a comparison; this is the number that
    /// moves.
    pub free_nll: f64,
    pub decisions: usize,
    pub forced: usize,
    /// Every scored decision, kept so a report can be re-cut without re-running
    /// a scoring pass that costs minutes.
    pub scored: Vec<Decision>,
    /// Decisions where the oracle did not admit what a human actually wrote.
    ///
    /// Should be zero. A nonzero count means the oracle and the parser disagree,
    /// which is a `stitch` bug, and it would otherwise arrive as an unexplained
    /// infinity in the mean.
    pub rejected_by_oracle: Vec<Decision>,
}

impl Report {
    /// Perplexity over free decisions — the same number as [`Self::free_nll`],
    /// in the units papers quote.
    #[must_use]
    pub fn free_perplexity(&self) -> f64 {
        self.free_nll.exp()
    }

    /// Mean free NLL over decisions in the first `tokens` of their program.
    ///
    /// The regime check. A rung whose early score is competitive and whose
    /// overall score is not is being asked questions outside the range it was
    /// built for — which is a different finding from "this rung is bad", and
    /// they are indistinguishable without this cut.
    #[must_use]
    pub fn free_nll_before(&self, tokens: u32) -> f64 {
        let early: Vec<f64> = self
            .scored
            .iter()
            .filter(|decision| !decision.is_forced() && decision.emitted < tokens)
            .map(|decision| decision.nll)
            .collect();
        if early.is_empty() { 0.0 } else { early.iter().sum::<f64>() / early.len() as f64 }
    }
}

/// Score `rung` on `programs`, which it must never have trained on.
///
/// A decision is made once before every token and once at the end for the
/// choice to stop — the same positions a decoder would face, which is what
/// makes this measure the thing the model is actually for.
#[must_use]
pub fn score<P: Predictor + ?Sized>(rung: &P, programs: &[String]) -> Report {
    let mut decisions = Vec::new();
    let mut rejected = Vec::new();

    for (index, program) in programs.iter().enumerate() {
        // Tracked as the walk proceeds rather than recovered per position: the
        // recovering version re-lexed the prefix every time, which is quadratic
        // in program length.
        let mut emitted = 0u32;
        let mut depth = 0u32;

        for token in stitch::lexer::lex(program).tokens {
            let position = token.span.start;
            let actual = class_of(&token.kind);
            let legal = valid_next_in(program, position, Entry::Program);

            if !legal.contains(actual) {
                rejected.push(Decision {
                    program: index,
                    position,
                    emitted,
                    actual,
                    legal_count: legal.len(),
                    nll: 0.0,
                });
                continue;
            }

            let at = Context { prefix: &program[..position], emitted, depth };
            let weights = rung.weights(at, legal);
            let total: f64 = weights.iter().map(|(_, weight)| *weight).sum();
            let weight = weights
                .iter()
                .find(|(class, _)| *class == actual)
                .map_or(0.0, |(_, weight)| *weight);

            decisions.push(Decision {
                program: index,
                position,
                emitted,
                actual,
                legal_count: legal.len(),
                nll: -(weight / total).ln(),
            });

            if actual != TokenClass::Eof {
                emitted += 1;
                if babble::is_obligation(actual) {
                    depth += 1;
                } else if babble::is_closer(actual) {
                    depth = depth.saturating_sub(1);
                }
            }
        }
    }

    let mean = |values: &[f64]| -> f64 {
        if values.is_empty() { 0.0 } else { values.iter().sum::<f64>() / values.len() as f64 }
    };
    let all: Vec<f64> = decisions.iter().map(|d| d.nll).collect();
    let free: Vec<f64> = decisions.iter().filter(|d| !d.is_forced()).map(|d| d.nll).collect();

    Report {
        rung: rung.name().to_string(),
        masked_nll: mean(&all),
        free_nll: mean(&free),
        decisions: decisions.len(),
        forced: decisions.len() - free.len(),
        scored: decisions,
        rejected_by_oracle: rejected,
    }
}
