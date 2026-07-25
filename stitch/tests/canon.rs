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
