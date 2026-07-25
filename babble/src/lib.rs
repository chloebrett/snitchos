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

/// Pick a class uniformly from those the oracle admits after `source`, without
/// computing the whole legal set.
///
/// Shuffling the candidates and taking the first viable one is *exactly* a
/// uniform draw from the viable ones — and it stops after roughly
/// `58 / |legal|` queries instead of always making 58. Since every query is a
/// parse, that is the difference between a walk that costs seconds and one
/// that costs tens of seconds.
fn pick(source: &str, rng: &mut Rng) -> Option<TokenClass> {
    let mut order: Vec<TokenClass> = all_classes().to_vec();
    // Fisher-Yates, seeded: the shuffle is part of the reproducible draw.
    for i in (1..order.len()).rev() {
        order.swap(i, rng.below(i + 1));
    }
    order
        .into_iter()
        .find(|class| admits_next(source, source.len(), *class, Entry::Program))
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
    let mut rng = Rng::new(seed);
    let mut source = String::new();
    let mut steps = Vec::new();
    for _ in 0..MAX_TOKENS {
        let Some(class) = pick(&source, &mut rng) else {
            break; // a dead prefix: unreachable from a legal emission
        };
        steps.push(Step { source_before: source.clone(), class });
        let Some(lexeme) = representative(class) else {
            break; // Eof: the program is complete
        };
        // The separator keeps this a walk over *tokens*: without it adjacent
        // lexemes would munch together into a different token stream than the
        // one the oracle approved.
        if !source.is_empty() {
            source.push(' ');
        }
        source.push_str(lexeme);
    }
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
