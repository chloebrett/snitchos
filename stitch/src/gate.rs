//! The corpus gate — parse → type → run the program's own tests.
//!
//! Used by the corpus pipeline (`plans/legacy/corpus-mvp.md`) to decide whether a
//! generated candidate is a training token or garbage, and by `bin/check.rs` to
//! answer the same question about a file on disk.
//!
//! **The funnel is the product.** A single pass/fail collapses four different
//! actions into one shrug, so [`Outcome`] keeps the stage a candidate died at:
//! parse deaths mean the generator does not know the grammar (an exemplar
//! problem), type deaths mean it knows the shape but not the semantics, test
//! deaths mean it had the semantics and got them wrong.
//!
//! The chain matches `tests/canon.rs` exactly, so the gate a candidate faces is
//! the gate the shipped corpus already passes.

use alloc::string::String;
use alloc::vec::Vec;

use crate::check::{Severity, check_program};
use crate::lower::lower_items_to_core;
use crate::parser::parse_program;
use crate::test_runner::run_tests;

/// Where a candidate stopped, and what it said on the way out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Did not parse. Carries the parse error rendered for a human.
    Parse(String),
    /// Parsed, but the checker reported at least one `Severity::Error`.
    Type(Vec<String>),
    /// Parsed and type-checked, but at least one of its own tests failed.
    Tests { failed: Vec<String>, passed: usize },
    /// Survived every stage. `tests` is how many ran — zero is allowed.
    Ok { tests: usize },
}

impl Outcome {
    /// Did the candidate survive the whole funnel?
    #[must_use]
    pub fn accepted(&self) -> bool {
        matches!(self, Outcome::Ok { .. })
    }

    /// The stage a candidate died at, as a lowercase word — the key the
    /// per-batch report groups on.
    #[must_use]
    pub fn stage(&self) -> &'static str {
        match self {
            Outcome::Parse(_) => "parse",
            Outcome::Type(_) => "type",
            Outcome::Tests { .. } => "tests",
            Outcome::Ok { .. } => "ok",
        }
    }
}

/// Run `src` through the full gate.
///
/// Type errors are filtered to [`Severity::Error`]: Stitch's checking is
/// gradual, so warnings are advisory and must not fail a candidate that is
/// otherwise fine.
#[must_use]
pub fn run(src: &str) -> Outcome {
    let items = match parse_program(src) {
        Ok(items) => items,
        Err(error) => return Outcome::Parse(alloc::format!("{error:?}")),
    };

    let type_errors: Vec<String> = check_program(&lower_items_to_core(&items))
        .into_iter()
        .filter(|error| error.severity == Severity::Error)
        .map(|error| error.message.clone())
        .collect();
    if !type_errors.is_empty() {
        return Outcome::Type(type_errors);
    }

    let results = run_tests(&items);
    let failed: Vec<String> = results
        .iter()
        .filter(|result| !result.passed())
        .map(|result| result.name.clone())
        .collect();
    if failed.is_empty() {
        return Outcome::Ok { tests: results.len() };
    }
    Outcome::Tests { passed: results.len() - failed.len(), failed }
}
