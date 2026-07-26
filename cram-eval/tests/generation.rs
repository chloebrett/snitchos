//! The generative metrics, and the two cuts that keep them honest.

use cram_eval::generate::{Generator, parse_rate};

#[test]
fn babble_parses_every_time() {
    // Rung 0's defining property, restated as an eval: babble cannot emit
    // syntactically invalid Stitch, so its parse rate is 100% by construction.
    // This is exactly why `unconstrained-parse%` is NOT a babble comparison —
    // a trained rung can only tie or lose. The test guards the *harness*: a
    // parse rate that scores babble below 100% is measuring something other
    // than what it claims to.
    let rate = parse_rate(&cram_eval::generate::Babble, 64);
    assert_eq!(
        rate.as_sampled, rate.samples,
        "babble emitted something that does not parse; first failing example: {:?}",
        rate.examples.iter().find(|example| !example.parses).map(|example| &example.text)
    );
    assert_eq!(rate.complete_items, rate.samples);
}

#[test]
fn a_rate_always_arrives_with_samples_beside_it() {
    // A bare `0.0%` once sent someone hunting through a backward pass that was
    // correct all along — the bug was a corpus separator, visible in one glance
    // at the output. No number this ladder prints travels alone.
    let rate = parse_rate(&cram_eval::generate::Babble, 8);
    assert!(!rate.examples.is_empty(), "a parse rate with no samples beside it");
    for example in &rate.examples {
        assert!(
            stitch::parser::parse_program(&example.text).is_ok() == example.parses,
            "an example's verdict disagrees with the parser"
        );
    }
}

#[test]
fn a_budget_stopped_rung_is_not_punished_for_stopping_mid_line() {
    // The distinction the two cuts exist for. This rung emits one whole item, a
    // blank line, then a fragment — exactly the shape of a model that ran out of
    // token budget. "As sampled" should fail it; "complete items" should not.
    struct Truncated;
    impl Generator for Truncated {
        fn name(&self) -> &str {
            "truncated"
        }
        fn stops_at_budget(&self) -> bool {
            true
        }
        fn sample(&self, _seed: u64) -> String {
            String::from("let count = 1\n\nlet total = count +\n")
        }
    }

    let rate = parse_rate(&Truncated, 4);
    assert_eq!(rate.as_sampled, 0, "the trailing fragment does not parse");
    assert_eq!(rate.complete_items, 4, "cutting back to the last item should parse");
}

#[test]
fn a_rung_that_finishes_is_judged_on_what_it_emitted() {
    // The other side of the same call: a rung that stops at a program boundary
    // must not have its last line cut off. Applying the budget cuts to babble
    // would chop a legal program and report a false failure.
    struct Whole;
    impl Generator for Whole {
        fn name(&self) -> &str {
            "whole"
        }
        fn sample(&self, _seed: u64) -> String {
            // One item, no blank line — cutting back to the last "\n\n" would
            // leave nothing at all.
            String::from("let count = 1\n")
        }
    }

    let rate = parse_rate(&Whole, 4);
    assert_eq!(rate.as_sampled, 4);
    assert_eq!(rate.complete_items, 4, "a whole program must not be cut back to nothing");
}
