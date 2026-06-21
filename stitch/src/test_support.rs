//! Shared helpers for the interpreter's unit tests, used across the modules
//! that split out of `interp` (`natives`, …). All drive the public API.

use crate::env::Env;
use crate::interp::{eval, eval_program, eval_program_with_telemetry};
use crate::parser::{parse, parse_program};
use crate::value::{TelemetryEvent, Value};

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

/// Parse and run a program, returning the telemetry it emitted.
pub(crate) fn run_program_events(src: &str) -> Vec<TelemetryEvent> {
    let items = parse_program(src).expect("test program should parse");
    let (result, events) = eval_program_with_telemetry(&items);
    result.expect("test program should evaluate");
    events
}
