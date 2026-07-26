//! The canon: every Stitch program the repo ships.
//!
//! These programs are simultaneously four things — userland, documentation
//! examples, regression fixtures, and the highest-value stratum of the training
//! corpus (see `docs/generative-ladder.md`, "The canon stratum"). The corpus
//! role is what this file exists for: bulk-generated Tier-0 text is validated by
//! construction, but the canon is hand-written, so *it* is the part that can
//! silently rot into something no model should imitate.
//!
//! The itests already run some of these under a booted kernel. That is the right
//! validation and the wrong feedback loop for authoring — it takes seconds per
//! program and covers only the ones a scenario names. This is the host-level
//! gate: every shipped `.st` file parses and type-checks clean, in milliseconds,
//! whether or not anything boots it.

use std::path::{Path, PathBuf};

use stitch::check::{Severity, check_program};
use stitch::lower::lower_items_to_core;
use stitch::parser::parse_program;
use stitch::test_runner::run_tests;

#[test]
fn every_shipped_program_parses() {
    for path in canon() {
        let src = read(&path);
        parse_program(&src).unwrap_or_else(|e| panic!("{} should parse: {e:?}", path.display()));
    }
}

/// Type checking is *gradual* — a diagnostic is advisory and never stops a run —
/// which is exactly why it needs a gate. Nothing else fails when a canon program
/// starts reporting errors, so without this the canon degrades quietly and the
/// corpus inherits the degradation.
#[test]
fn every_shipped_program_type_checks_without_errors() {
    let mut offenders = Vec::new();
    for path in canon() {
        let items = parse_program(&read(&path)).expect("canon parses");
        let errors: Vec<String> = check_program(&lower_items_to_core(&items))
            .into_iter()
            .filter(|error| error.severity == Severity::Error)
            .map(|error| error.message.clone())
            .collect();
        if !errors.is_empty() {
            offenders.push(format!("{}: {}", path.display(), errors.join("; ")));
        }
    }
    assert!(offenders.is_empty(), "type errors in the canon:\n{}", offenders.join("\n"));
}

/// The gate above passes on arrival, which is only reassuring if it can fail.
/// Gradual typing makes that a live question: a checker that returned nothing
/// would look identical from the outside. This is the control — a program with
/// an error the canon must never contain, asserted to be caught by the same
/// call the gate makes.
#[test]
fn the_type_gate_catches_an_error_when_there_is_one() {
    let items = parse_program("f() -> Int = \"not an int\"").expect("control program parses");

    let errors: Vec<Severity> = check_program(&lower_items_to_core(&items))
        .into_iter()
        .map(|error| error.severity)
        .collect();

    assert!(
        errors.contains(&Severity::Error),
        "the type gate reports nothing for a return-type mismatch: {errors:?}"
    );
}

/// Every native `test` declaration the canon ships must pass.
///
/// This is the gate that makes the runner load-bearing rather than merely
/// available: without it a canon program's own tests could rot with nothing
/// failing. Rust's job here is to *drive* the suite, not to author it — the
/// assertions live in the `.st` files, in Stitch.
#[test]
fn every_shipped_programs_native_tests_pass() {
    let mut failures = Vec::new();
    for path in canon() {
        let items = parse_program(&read(&path)).expect("canon parses");
        for result in run_tests(&items) {
            if !result.passed() {
                failures.push(format!("{}: {} — {:?}", path.display(), result.name, result.verdict));
            }
        }
    }
    assert!(failures.is_empty(), "native tests failed:\n{}", failures.join("\n"));
}

/// A green gate over *zero* tests is indistinguishable from a green gate over a
/// working suite, and that is the state this gate shipped in — the runner
/// existed before any canon program used it. A floor that ratchets, like
/// `the_canon_is_not_empty`: raise it as tranches land, never lower it.
#[test]
fn the_canon_carries_native_tests() {
    let found: usize = canon()
        .iter()
        .map(|path| run_tests(&parse_program(&read(path)).expect("canon parses")).len())
        .sum();

    assert!(found >= 6, "expected the canon's native suites, found {found}");
}

/// The gate above passes on arrival, which is only reassuring if it can fail —
/// the same control `the_type_gate_catches_an_error_when_there_is_one` provides
/// for the type stage, and for the same reason: a runner that silently found no
/// tests would look identical from the outside.
#[test]
fn the_native_test_gate_catches_a_failing_test() {
    let items = parse_program(r#"test "deliberately wrong" { expect 1 == 2 }"#)
        .expect("control program parses");

    let results = run_tests(&items);

    assert_eq!(results.len(), 1, "the runner should find the control's test");
    assert!(!results[0].passed(), "a false assertion must not pass");
}

/// The canon's value to the corpus is that it is *real Stitch as we write it*.
/// A canon that shrinks to a handful of toys stops being that, so the count is
/// asserted — a floor that ratchets, not a target.
#[test]
fn the_canon_is_not_empty() {
    let canon = canon();
    assert!(
        canon.len() >= 5,
        "expected the shipped .st corpus, found {canon:?}"
    );
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).expect("canon source should be readable")
}

/// Every `.st` file the repo ships: `fs-image/` (what seeds ramfs) plus the
/// interpreter's own prelude.
fn canon() -> Vec<PathBuf> {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("stitch/ has a parent")
        .to_path_buf();
    let mut found = vec![repo.join("stitch/src/prelude.st")];
    collect_st(&repo.join("fs-image"), &mut found);
    found.sort();
    found
}

fn collect_st(dir: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_st(&path, found);
        } else if path.extension().is_some_and(|e| e == "st") {
            found.push(path);
        }
    }
}
