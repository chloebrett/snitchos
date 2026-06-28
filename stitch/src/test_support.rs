//! Shared helpers for the interpreter's unit tests, used across the modules
//! that split out of `interp` (`natives`, …). All drive the public API.

#[allow(clippy::wildcard_imports, reason = "alloc prelude for no_std")]
use crate::prelude::*;

use crate::env::Env;
use crate::interp::{
    Module, eval, eval_modules, eval_program, eval_program_with_platform,
    eval_program_with_telemetry,
};
use crate::parser::{parse, parse_program};
use crate::platform::Platform;
use crate::value::{RuntimeError, TelemetryEvent, Value};

/// Parse then evaluate in an empty environment, unwrapping — for tests with
/// valid, total expressions.
pub(crate) fn run(src: &str) -> Value {
    eval(&parse(src).expect("test input should parse"), &Env::new())
        .expect("test input should evaluate")
}

/// Parse then evaluate, expecting a runtime error message.
pub(crate) fn run_err(src: &str) -> String {
    eval(&parse(src).expect("test input should parse"), &Env::new())
        .expect_err("test input should fail at runtime")
        .message()
}

/// Parse a whole program (top-level items) and run its `main`.
pub(crate) fn run_program(src: &str) -> Value {
    let items = parse_program(src).expect("test program should parse");
    eval_program(&items).expect("test program should evaluate")
}

/// Parse and run a program, expecting a runtime error message.
pub(crate) fn run_program_err(src: &str) -> String {
    let items = parse_program(src).expect("test program should parse");
    eval_program(&items)
        .expect_err("test program should fail at runtime")
        .message()
}

/// Parse a set of named modules (`(name, source)`) and run the entry module's
/// `main`. The module set is the loadable unit — built in-memory here, from the
/// filesystem in the CLI.
pub(crate) fn run_modules(sources: &[(&str, &str)], entry: &str) -> Value {
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
pub(crate) fn run_program_on(
    src: &str,
    platform: Rc<dyn Platform>,
) -> Result<Value, RuntimeError> {
    let items = parse_program(src).expect("test program should parse");
    eval_program_with_platform(&items, platform)
}

/// Parse and run a program, returning the telemetry it emitted.
pub(crate) fn run_program_events(src: &str) -> Vec<TelemetryEvent> {
    let items = parse_program(src).expect("test program should parse");
    let (result, events) = eval_program_with_telemetry(&items);
    result.expect("test program should evaluate");
    events
}
