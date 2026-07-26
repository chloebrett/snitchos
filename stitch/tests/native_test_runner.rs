//! The native test runner: `test` items in, verdicts out.
//!
//! The runner is a *pure function* over parsed items — no I/O, no printing, no
//! process exit. That is what lets the same code drive `cargo xtask test` on the
//! host, the funnel's run stage (`plans/stage-0-validator-funnel.md`), and a
//! `stitch test` verb on the metal, where there is no stdout to print to.

use stitch::parser::parse_program;
use stitch::test_runner::{Verdict, run_tests};

fn results(src: &str) -> Vec<stitch::test_runner::TestResult> {
    run_tests(&parse_program(src).expect("test program should parse"))
}

/// The runner's whole contract in one program: every `test` item runs, in source
/// order, and each reports on itself independently — one failure does not stop
/// the ones after it, because a suite that stops at the first failure tells you
/// about one problem when you have three.
#[test]
fn every_test_runs_and_reports_independently() {
    let results = results(
        r#"
        test "passes" { expect 1 == 1 }
        test "fails" { expect 1 == 2 }
        test "faults" { expect 1 / 0 == 1 }
        "#,
    );

    let names: Vec<&str> = results.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["passes", "fails", "faults"]);
    assert!(matches!(results[0].verdict, Verdict::Passed));
    assert!(matches!(results[1].verdict, Verdict::Failed { .. }));
    assert!(matches!(results[2].verdict, Verdict::Failed { .. }));
}

/// A failure has to be actionable without re-running anything: which assertion,
/// what the operands were, and where it is.
#[test]
fn a_failure_carries_the_rendered_operands_and_a_location() {
    let results = results(r#"test "arithmetic" { expect 1 + 1 == 3 }"#);
    match &results[0].verdict {
        Verdict::Failed { message, span } => {
            assert!(message.contains('2'), "left operand missing: {message}");
            assert!(message.contains('3'), "right operand missing: {message}");
            assert!(span.is_some(), "a failure should cite where it happened");
        }
        other => panic!("expected a failure, got {other:?}"),
    }
}

/// Tests call the module they are written beside — that is the point of them
/// living in the same file.
#[test]
fn a_test_calls_the_functions_declared_alongside_it() {
    let results = results(
        r#"
        double(n) = n * 2
        test "doubles" { expect double(21) == 42 }
        "#,
    );
    assert!(matches!(results[0].verdict, Verdict::Passed), "{:?}", results[0].verdict);
}

/// A generated candidate can loop forever and the funnel must not. Exhaustion is
/// its own verdict, not a generic fault: "this test never finished" is a
/// different diagnosis from "this test was wrong", and the funnel reports the
/// stage a candidate died at.
#[test]
fn a_nonterminating_test_is_reported_as_exhausted_not_as_a_hang() {
    let results = results(
        r#"
        spin(n) = spin(n + 1)
        test "loops" { expect spin(0) == 1 }
        "#,
    );
    assert!(
        matches!(results[0].verdict, Verdict::Exhausted),
        "expected exhaustion, got {:?}",
        results[0].verdict
    );
}

/// The design's headline, at runtime: a test's `uses` clause *is* its authority.
/// Declaring nothing means holding nothing, so a test is pure — and therefore
/// deterministic — by construction rather than by convention.
#[test]
fn a_test_that_declares_no_authority_cannot_perform_an_effect() {
    let results = results(r#"test "sneaks" { emit("metric", 1)  expect true }"#);
    match &results[0].verdict {
        Verdict::Failed { message, .. } => {
            assert!(message.contains("Telemetry"), "should name the missing cap: {message}");
        }
        other => panic!("an undeclared effect should be refused, got {other:?}"),
    }
}

/// ...and the converse, or the rule above would be satisfied by a runner that
/// simply grants nothing to anyone.
#[test]
fn a_test_that_declares_authority_may_perform_that_effect() {
    let results = results(r#"test "emits" uses Telemetry { emit("metric", 1)  expect true }"#);
    assert!(matches!(results[0].verdict, Verdict::Passed), "{:?}", results[0].verdict);
}

/// The migration of `stim_fsm.rs` turns on this: its Rust helpers project a
/// variant down to a string tag (`match st.state.mode { Normal => "Normal" … }`)
/// because Rust could not name a Stitch variant. Natively the variant can be
/// compared directly — nullary variants are singleton values, so `==` is
/// structural and means what it looks like.
#[test]
fn a_nullary_variant_compares_by_equality() {
    let results = results(
        r#"
        sum Mode = Normal | Insert
        current() -> Mode = Normal
        test "variant equality" {
            expect current() == Normal
            expect not (current() == Insert)
        }
        "#,
    );
    assert!(matches!(results[0].verdict, Verdict::Passed), "{:?}", results[0].verdict);
}

/// The other half, and the reason the port is not a blanket rewrite: a variant
/// *carrying* a payload is a constructor when named bare, so "is it a `Save`,
/// whatever it holds" is still a `match` rather than an `==`.
#[test]
fn a_payload_carrying_variant_is_matched_not_compared() {
    let results = results(
        r#"
        sum Effect = Save(Str) | Noop
        act() -> Effect = Save("buffer")
        test "variant tag" {
            expect match act() { Save(_) => true  Noop => false }
        }
        "#,
    );
    assert!(matches!(results[0].verdict, Verdict::Passed), "{:?}", results[0].verdict);
}

/// A program with no tests is not a failure — it is a program with no tests.
/// The funnel is what decides that an untested candidate is worthless; the
/// runner just reports what it found.
#[test]
fn a_program_without_tests_yields_no_results() {
    assert!(results("main() = 1").is_empty());
}

/// A test runs in the program's own environment, so the stdlib is reachable on
/// the same terms as anywhere else — `Str` is a built-in *module*, so it needs
/// the same `use` a function body would need, and the test's `use` is the
/// module's.
#[test]
fn the_stdlib_is_reachable_from_a_test() {
    let results = results(
        r#"
        use Str
        test "stdlib" { expect Str.length("abc") == 3 }
        "#,
    );
    assert!(matches!(results[0].verdict, Verdict::Passed), "{:?}", results[0].verdict);
}
