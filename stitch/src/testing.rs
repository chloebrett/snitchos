//! Shared harness for running Stitch programs from Rust. Used by the crate's own
//! unit tests (across the modules split out of `interp` — `natives`, …), its
//! integration tests, and external consumers (stim's FSM tests, the Stitch
//! mutation tester) via the `testing` feature. Every helper drives the public
//! API (`parse_program` + the `eval_*` entry points) and unwraps for terse
//! assertions — panics are the point.

#[allow(clippy::wildcard_imports, reason = "alloc prelude for no_std")]
use crate::prelude::*;

use crate::env::Env;
use crate::interp::{
    Module, eval, eval_modules, eval_program, eval_program_with_platform,
    eval_program_with_telemetry,
};
use crate::lower::lower_expr_to_core;
use crate::parser::{parse, parse_program};
use crate::platform::Platform;
use crate::value::{RuntimeError, TelemetryEvent, Value};

/// Parse then evaluate in an empty environment, unwrapping — for tests with
/// valid, total expressions.
pub fn run(src: &str) -> Value {
    let expr = parse(src).expect("test input should parse");
    eval(&lower_expr_to_core(&expr), &Env::new()).expect("test input should evaluate")
}

/// Parse then evaluate, expecting a runtime error message.
pub fn run_err(src: &str) -> String {
    let expr = parse(src).expect("test input should parse");
    eval(&lower_expr_to_core(&expr), &Env::new())
        .expect_err("test input should fail at runtime")
        .message()
}

/// Parse a whole program (top-level items) and run its `main`.
pub fn run_program(src: &str) -> Value {
    let items = parse_program(src).expect("test program should parse");
    eval_program(&items).expect("test program should evaluate")
}

/// Parse and run a program, expecting a runtime error message.
pub fn run_program_err(src: &str) -> String {
    let items = parse_program(src).expect("test program should parse");
    eval_program(&items)
        .expect_err("test program should fail at runtime")
        .message()
}

/// Run a body expression against a set of top-level `defs` (which may `use`
/// builtin modules like `Str`/`List`), returning its value. Synthesizes
/// `main() = {body}`, appends it to `defs`, and evaluates through the module
/// path so builtin-module imports resolve. The convenience external harnesses
/// build on — load a `.st` program (`defs`) then exercise one of its functions
/// (`body`) — e.g. `run_source(STIM, r#"initialState("a\nb")"#)`.
pub fn run_source(defs: &str, body: &str) -> Value {
    let src = format!("{defs}\nmain() = {body}");
    run_modules(&[("main", src.as_str())], "main")
}

/// Parse a set of named modules (`(name, source)`) and run the entry module's
/// `main`. The module set is the loadable unit — built in-memory here, from the
/// filesystem in the CLI.
pub fn run_modules(sources: &[(&str, &str)], entry: &str) -> Value {
    let modules = sources
        .iter()
        .map(|(name, src)| Module {
            name: (*name).to_string(),
            items: parse_program(src).expect("test module should parse"),
        })
        .collect::<Vec<_>>();
    eval_modules(&modules, entry).expect("test modules should evaluate")
}

/// Parse and run a program against an installed [`Platform`] backend, returning
/// `main`'s result (`Ok`) or its runtime error (`Err`) — for asserting on a
/// program's console / cap effects (and on refused, undeclared effects).
pub fn run_program_on(
    src: &str,
    platform: Rc<dyn Platform>,
) -> Result<Value, RuntimeError> {
    let items = parse_program(src).expect("test program should parse");
    eval_program_with_platform(&items, platform)
}

/// Parse and run a program, returning the telemetry it emitted.
pub fn run_program_events(src: &str) -> Vec<TelemetryEvent> {
    let items = parse_program(src).expect("test program should parse");
    let (result, events) = eval_program_with_telemetry(&items);
    result.expect("test program should evaluate");
    events
}
