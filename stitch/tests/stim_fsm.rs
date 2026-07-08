//! Integration tests for the stim editor FSM (`fs-image/stim.st`), exercised
//! through the real Stitch interpreter via `stitch::testing`. The FSM is a
//! Stitch *program*: these tests load it and drive its pure transitions,
//! asserting on the returned state — no I/O, no driver loop.

use stitch::testing::run_source;
use stitch::value::Value;

/// The canonical FSM source — the same file the ramfs seeds (at `/stim/stim.st`)
/// and the shell loads.
const STIM: &str = include_str!("../../fs-image/stim/stim.st");

/// Evaluate `body` against the stim FSM's definitions, returning its value.
fn fsm(body: &str) -> Value {
    run_source(STIM, body)
}

#[test]
fn initial_state_splits_text_into_a_line_buffer_at_origin() {
    // "a\nb" → two lines, cursor at the origin, Normal mode.
    insta::assert_debug_snapshot!(fsm(r#"initialState("a\nb")"#));
}

#[test]
fn initial_state_of_empty_text_is_one_empty_line() {
    // The empty document is one empty line, not zero lines — a cursor always
    // has a line to sit on.
    insta::assert_debug_snapshot!(fsm(r#"initialState("")"#));
}
