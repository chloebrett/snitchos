//! Running the `test` declarations in a Stitch program.
//!
//! The runner is a **pure function over parsed items**: items in, verdicts out.
//! No printing, no I/O, no process exit. That is what lets one implementation
//! serve three callers that share nothing else — the host gate (`cargo xtask
//! test`), the corpus funnel's run stage (`plans/stage-0-validator-funnel.md`),
//! and a `stitch test` verb on the metal, where there is no stdout to print to.
//!
//! Two properties are load-bearing rather than incidental:
//!
//! - **Every test runs.** A suite that stops at the first failure reports one
//!   problem when you have three.
//! - **Every test is bounded.** A generated candidate can loop forever and the
//!   funnel must not, so exhaustion is a verdict rather than a hang — and its
//!   own verdict, because "never finished" and "was wrong" call for different
//!   fixes.
//!
//! Authority comes from the declaration: a test's `uses` clause *is* its
//! authority, so a test that declares nothing can perform nothing. The checker
//! already rejects an undeclared effect statically; this is the runtime half of
//! the same promise.

#[allow(clippy::wildcard_imports, reason = "alloc prelude for no_std")]
use crate::prelude::*;

use crate::ast::Item;
use crate::interp::{FUEL_EXHAUSTED, build_env, eval, prelude_items};
use crate::lexer::Span;
use crate::lower::lower_expr_to_core;

/// Evaluation steps a single test may spend before it is called non-terminating.
///
/// Generous for anything a test should be doing (the canon's heaviest pure
/// functions are orders of magnitude under it) and small enough that a spinning
/// candidate costs a moment rather than a wedged run.
pub const DEFAULT_FUEL: u64 = 1_000_000;

/// What happened to one test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Passed,
    /// An assertion failed, or the body faulted. `message` is the fault's own
    /// text — for a failed `expect` that includes both rendered operands.
    Failed { message: String, span: Option<Span> },
    /// The test spent its whole budget without finishing.
    Exhausted,
}

/// One test's name and what happened to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestResult {
    pub name: String,
    pub verdict: Verdict,
}

impl TestResult {
    #[must_use]
    pub fn passed(&self) -> bool {
        matches!(self.verdict, Verdict::Passed)
    }
}

/// Run every `test` declaration in `items`, in source order, under
/// [`DEFAULT_FUEL`].
#[must_use]
pub fn run_tests(items: &[Item]) -> Vec<TestResult> {
    run_tests_with_fuel(items, DEFAULT_FUEL)
}

/// Run every `test` declaration in `items` under a budget of `fuel` steps each.
///
/// The environment — prelude, natives, the program's own declarations — is built
/// **once** and refuelled per test, so a file with many tests pays the (large)
/// registration cost once rather than per test.
#[must_use]
pub fn run_tests_with_fuel(items: &[Item], fuel: u64) -> Vec<TestResult> {
    let mut all = prelude_items();
    all.extend_from_slice(items);
    let env = build_env(&all);

    items
        .iter()
        .filter_map(|item| match item {
            Item::Test { name, uses, body } => {
                // A test's declared `uses` *is* its authority: no clause, no
                // authority. The program env holds the ambient set, so this
                // narrows rather than grants — the same move a named function's
                // `uses` makes at a call.
                let authority = uses.iter().map(|effect| effect.name.clone()).collect();
                let test_env = env.clone().with_authority(authority);
                test_env.refuel(fuel);
                let verdict = match eval(&lower_expr_to_core(body), &test_env) {
                    Ok(_) => Verdict::Passed,
                    Err(error) if error.message() == FUEL_EXHAUSTED => Verdict::Exhausted,
                    Err(error) => Verdict::Failed {
                        message: error.message(),
                        span: error.span(),
                    },
                };
                Some(TestResult { name: name.clone(), verdict })
            }
            _ => None,
        })
        .collect()
}
