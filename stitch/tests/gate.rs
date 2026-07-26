//! The corpus gate: does a candidate program survive parse → type → its own tests?
//!
//! This is the funnel from `plans/corpus-mvp.md` in one function. The *stage* a
//! candidate dies at is the diagnosis — parse deaths mean the generator does not
//! know the grammar, type deaths mean it has the shape but not the semantics,
//! test deaths mean it has the semantics and got them wrong — so `Outcome`
//! distinguishes them rather than collapsing to a bool.

use stitch::gate::{self, Outcome};

const GOOD: &str = "\
ext double(n: Int) -> Int = n * 2

test \"double doubles\" { expect double(2) == 4 }
";

#[test]
fn a_clean_program_passes_and_reports_its_test_count() {
    match gate::run(GOOD) {
        Outcome::Ok { tests } => assert_eq!(tests, 1),
        other => panic!("expected Ok, got {other:?}"),
    }
}

#[test]
fn a_syntax_error_dies_at_parse() {
    let outcome = gate::run("ext double(n: Int -> Int = n * 2");
    assert!(matches!(outcome, Outcome::Parse(_)), "got {outcome:?}");
}

/// Type checking is gradual — a diagnostic never stops a run — so a program can
/// be perfectly parseable and still be wrong. Without this stage the gate would
/// wave through anything the parser accepted.
#[test]
fn a_type_error_dies_at_type() {
    let outcome = gate::run("f() -> Int = \"not an int\"");
    assert!(matches!(outcome, Outcome::Type(_)), "got {outcome:?}");
}

/// The stage that caught candidate 005: the program was valid Stitch and its own
/// test disagreed with it.
#[test]
fn a_failing_test_dies_at_tests_and_names_it() {
    let src = "\
ext double(n: Int) -> Int = n * 2

test \"double doubles\" { expect double(2) == 4 }
test \"this one is wrong\" { expect double(2) == 5 }
";
    match gate::run(src) {
        Outcome::Tests { failed, passed } => {
            assert_eq!(passed, 1);
            assert_eq!(failed, vec!["this one is wrong".to_string()]);
        }
        other => panic!("expected Tests, got {other:?}"),
    }
}

/// A program with no tests still passes the gate — the corpus wants the recipe
/// to ask for tests, not the gate to require them.
#[test]
fn a_program_with_no_tests_passes_with_a_count_of_zero() {
    match gate::run("ext double(n: Int) -> Int = n * 2") {
        Outcome::Ok { tests } => assert_eq!(tests, 0),
        other => panic!("expected Ok, got {other:?}"),
    }
}
