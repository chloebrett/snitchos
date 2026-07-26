//! What has to be true before any number this harness prints means anything.

use cram_eval::{Babble, Context, Predictor, Uniform, score};
use stitch::oracle::{Entry, TokenClass, valid_next_in};

/// A context at the end of `prefix`, with the walk state a scorer would have
/// tracked to get there.
fn at(prefix: &str) -> Context<'_> {
    let emitted = stitch::lexer::lex(prefix).tokens.len().saturating_sub(1) as u32;
    Context { prefix, emitted, depth: 0 }
}

/// Small and real — enough to exercise scoring without the corpus loader.
///
/// Every one of these is checked to parse by
/// [`the_fixture_is_actually_stitch`], because the first draft of this fixture
/// was not: `prod point { x y }` is invented syntax, and it surfaced as five
/// "the oracle rejected a human-written token" decisions. The oracle was right
/// and the human was me.
fn programs() -> Vec<String> {
    vec![
        String::from("let count = 1\n"),
        String::from("prod Config(host: Str, port: Int)\n"),
        String::from("let total = count + 2\n"),
    ]
}

#[test]
fn the_fixture_is_actually_stitch() {
    // Guards every other test in this file: a fixture that does not parse makes
    // the oracle-agreement check fail for a reason that has nothing to do with
    // the oracle.
    for program in programs() {
        assert!(
            stitch::parser::parse_program(&program).is_ok(),
            "fixture is not Stitch: {program:?}"
        );
    }
}

#[test]
fn every_legal_class_gets_positive_probability() {
    // A zero is an infinite NLL, and one such position swamps the mean of every
    // other decision in the corpus — the whole report becomes `inf` and says
    // nothing about where the rung is actually weak.
    let source = "let count = ";
    let legal = valid_next_in(source, source.len(), Entry::Program);
    for rung in predictors() {
        let weights = rung.weights(at(source), legal);
        assert_eq!(
            weights.len() as u32,
            legal.len(),
            "{}: published {} weights for {} legal classes",
            rung.name(),
            weights.len(),
            legal.len()
        );
        for (class, weight) in weights {
            assert!(weight > 0.0, "{}: {class:?} has weight {weight}", rung.name());
        }
    }
}

#[test]
fn no_rung_puts_weight_on_a_class_the_oracle_forbids() {
    // The mask is the correctness floor of this whole ladder: a rung that
    // scores an illegal class is one that would *emit* it, and the harness must
    // catch that here rather than at decode time.
    let source = "let count = ";
    let legal = valid_next_in(source, source.len(), Entry::Program);
    for rung in predictors() {
        for (class, _) in rung.weights(at(source), legal) {
            assert!(
                legal.contains(class),
                "{}: weight on {class:?}, which the oracle forbids here",
                rung.name()
            );
        }
    }
}

#[test]
fn the_oracle_admits_every_token_a_human_wrote() {
    // A drift check wearing a scoring hat. `score` cannot assign a probability
    // to a class the oracle rejects, so any such position would silently drop
    // out of the mean. If this count is ever nonzero the disagreement is
    // between `stitch`'s oracle and its parser, not in the model.
    let report = score(&Babble::default(), &programs());
    assert!(
        report.rejected_by_oracle.is_empty(),
        "oracle rejected human-written tokens: {:?}",
        report.rejected_by_oracle
    );
}

#[test]
fn a_forced_position_costs_every_rung_exactly_nothing() {
    // Where the grammar admits one class there is no prediction to make. Both
    // rungs must score 0 there, or the "free" mean is measuring the harness
    // rather than the rungs.
    let babble = score(&Babble::default(), &programs());
    let uniform = score(&Uniform, &programs());

    assert_eq!(babble.decisions, uniform.decisions, "same corpus, same decisions");
    assert_eq!(babble.forced, uniform.forced, "forcedness is the oracle's, not the rung's");
    assert!(babble.forced > 0, "this corpus should contain forced positions");

    // Every decision is scored, so the forced ones are visible in the gap
    // between the two means rather than hidden.
    assert!(
        babble.masked_nll < babble.free_nll,
        "forced positions score 0, so they can only pull the overall mean down"
    );
}

#[test]
fn scoring_is_deterministic() {
    // Nothing here samples. Two runs that disagree would mean the report is
    // reading state it should not have.
    let first = score(&Babble::default(), &programs());
    let second = score(&Babble::default(), &programs());
    assert_eq!(first.masked_nll.to_bits(), second.masked_nll.to_bits());
    assert_eq!(first.decisions, second.decisions);
}

#[test]
fn a_rung_that_knows_the_answer_scores_near_zero() {
    // The harness's own calibration: if a predictor that puts nearly all its
    // weight on what the human wrote does *not* score near zero, the scorer is
    // broken and every comparison it produces is noise. This is the control the
    // plan asks for, in its strongest form.
    struct Clairvoyant;
    impl Predictor for Clairvoyant {
        fn name(&self) -> &'static str {
            "clairvoyant"
        }
        fn weights(
            &self,
            at: Context<'_>,
            legal: stitch::oracle::TokenSet,
        ) -> Vec<(TokenClass, f64)> {
            // What the human wrote next, read straight off the program being
            // scored — cheating, deliberately, because a cheater's score is the
            // scale's zero.
            let actual = stitch::lexer::lex(WHOLE)
                .tokens
                .into_iter()
                .find(|token| token.span.start >= at.prefix.len())
                .map(|token| stitch::oracle::class_of(&token.kind));
            legal
                .to_vec()
                .into_iter()
                .map(|class| (class, if Some(class) == actual { 1e6 } else { 1.0 }))
                .collect()
        }
    }
    const WHOLE: &str = "let count = 1\n";

    let report = score(&Clairvoyant, &[String::from(WHOLE)]);
    assert!(
        report.free_nll < 0.01,
        "a predictor that knows the answer should score ~0, got {}",
        report.free_nll
    );
}

fn predictors() -> Vec<Box<dyn Predictor>> {
    vec![Box::new(Babble::default()), Box::new(Uniform)]
}
