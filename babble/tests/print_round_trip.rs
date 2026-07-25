//! babble's output, re-printed.
//!
//! babble renders a flat, space-separated token stream: no newlines, no
//! indentation, and operators padded (`> =`, `?.`) because each lexeme is an
//! independent oracle choice. Real Stitch looks nothing like that, so Tier-0
//! corpus text has to pass through the AST printer before it can join a training
//! mix — otherwise the model learns babble's *renderer* rather than the language.
//!
//! babble's output parses by construction, which makes the printer's round-trip
//! contract checkable over unlimited generated programs. This is the test that
//! says the two crates compose.

use stitch::parser::parse_program;
use stitch::print::print_program;

/// Programs checked by default — enough seeds to cross every construct babble's
/// tables reach, and ~2 ms each.
///
/// Raise it with `BABBLE_PRINT_SEEDS` for a deep sweep. The round-trip is a
/// property over an infinite generator, so the constant is a budget, not a
/// claim; rare token-adjacency hazards (`e?` before `.`, found at seed 227) only
/// surface at volume, and the sweep is how the next one gets found.
fn seeds() -> u64 {
    std::env::var("BABBLE_PRINT_SEEDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(500)
}

#[test]
fn every_generated_program_survives_a_print_round_trip() {
    for seed in 0..seeds() {
        let src = babble::generate(seed);
        let ast = parse_program(&src)
            .unwrap_or_else(|e| panic!("seed {seed} should parse by construction: {e:?}\n{src}"));
        let printed = print_program(&ast);
        let reparsed = parse_program(&printed).unwrap_or_else(|e| {
            panic!("printed seed {seed} should re-parse: {e:?}\n--- babble\n{src}\n--- printed\n{printed}")
        });
        assert_eq!(
            ast, reparsed,
            "seed {seed} changed shape\n--- babble\n{src}\n--- printed\n{printed}"
        );
    }
}

/// The point of the exercise, stated as the two distribution facts that make
/// babble's raw rendering unusable as training text.
///
/// Density is the honest measure of the padding: `( )`, `?. `, `> =` are each
/// legitimate somewhere (`queue<value> = …` really does contain `> =`), so
/// grepping for them yields false positives. What is *not* ambiguous is that
/// uniform space-separation inflates every program, and that the printer must
/// shrink it back even while adding newlines and indentation.
#[test]
fn printing_makes_babble_output_denser_than_its_padded_rendering() {
    let (babbled, printed) = (0..seeds()).fold((0_usize, 0_usize), |(b, p), seed| {
        let src = babble::generate(seed);
        let out = print_program(&parse_program(&src).expect("babble output parses"));
        (b + src.len(), p + out.len())
    });
    assert!(
        printed < babbled,
        "printing should compress babble's padding: {printed} bytes vs {babbled}"
    );
}

/// babble emits neither newlines nor indentation — its grammar has no reason
/// to. Layout is the other half of what the printer is for: a model trained on
/// flat token streams learns a language nobody writes.
#[test]
fn printing_introduces_the_layout_babble_never_emits() {
    let indented = (0..seeds())
        .filter(|&seed| {
            let src = babble::generate(seed);
            assert!(!src.contains('\n'), "babble emits no newlines");
            print_program(&parse_program(&src).expect("babble output parses")).contains("\n    ")
        })
        .count();
    assert!(indented > 0, "no seed produced an indented block");
}
