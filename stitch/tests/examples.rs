//! The examples corpus: every program under `examples/stitch/`.
//!
//! Distinct from `canon.rs` (`fs-image/` + the prelude — what's *shipped*,
//! parse+type-check only) and from `stim_fsm.rs` (one program's own domain
//! tests). This is the gate for `plans/stitch-examples-corpus.md`: each
//! example must parse, type-check clean, and — the property canon.rs doesn't
//! check — every `test` item inside it must pass. A program with no `test`
//! items is treated as a failure here; the whole point of the batch is
//! programs that assert their own behavior.

use std::path::{Path, PathBuf};

use stitch::check::{Severity, check_program};
use stitch::lower::lower_items_to_core;
use stitch::parser::parse_program;
use stitch::test_runner::{Verdict, run_tests};

#[test]
fn every_example_parses_and_type_checks_clean() {
    let mut offenders = Vec::new();
    for path in examples() {
        let src = read(&path);
        let items = match parse_program(&src) {
            Ok(items) => items,
            Err(e) => {
                offenders.push(format!("{}: parse error: {e:?}", path.display()));
                continue;
            }
        };
        let errors: Vec<String> = check_program(&lower_items_to_core(&items))
            .into_iter()
            .filter(|error| error.severity == Severity::Error)
            .map(|error| error.message.clone())
            .collect();
        if !errors.is_empty() {
            offenders.push(format!("{}: {}", path.display(), errors.join("; ")));
        }
    }
    assert!(offenders.is_empty(), "problems in the examples corpus:\n{}", offenders.join("\n"));
}

#[test]
fn every_example_has_tests_and_they_all_pass() {
    let mut offenders = Vec::new();
    for path in examples() {
        let items = parse_program(&read(&path)).expect("checked by the parse gate above");
        let results = run_tests(&items);
        if results.is_empty() {
            offenders.push(format!("{}: no `test` items", path.display()));
            continue;
        }
        for result in results {
            if !result.passed() {
                offenders.push(format!(
                    "{}: test \"{}\" did not pass: {:?}",
                    path.display(),
                    result.name,
                    result.verdict
                ));
            }
        }
    }
    assert!(offenders.is_empty(), "failing examples:\n{}", offenders.join("\n"));
}

/// A control, same reason `canon.rs` has one: a runner that reports nothing
/// would pass the assertion above vacuously.
#[test]
fn the_test_gate_catches_a_failing_test_when_there_is_one() {
    let items = parse_program(r#"test "control" { expect 1 == 2 }"#).expect("control parses");
    let results = run_tests(&items);
    assert!(matches!(results[0].verdict, Verdict::Failed { .. }));
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).expect("example source should be readable")
}

fn examples() -> Vec<PathBuf> {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("stitch/ has a parent")
        .to_path_buf();
    let mut found = Vec::new();
    collect_st(&repo.join("examples/stitch"), &mut found);
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
