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

/// Project `[row, col]` from the `Editor` that `setup` evaluates to — the
/// cursor is what the `j`/`k` transitions move. Binding `rc` keeps the trailing
/// `[…]` off the prior call (maximal-munch would read it as an index).
fn row_col(setup: &str) -> Value {
    fsm(&format!("{{ let s = {setup}  let rc = [s.row, s.col]  rc }}"))
}

fn ints(a: i64, b: i64) -> Value {
    Value::List(vec![Value::Int(a), Value::Int(b)].into())
}

#[test]
fn j_moves_down_a_line_and_k_moves_up() {
    // Three lines; j descends 0→1, k climbs back 1→0.
    assert_eq!(row_col(r#"moveDown(initialState("a\nb\nc"))"#), ints(1, 0));
    assert_eq!(row_col(r#"moveUp(moveDown(initialState("a\nb\nc")))"#), ints(0, 0));
}

#[test]
fn j_at_the_last_line_and_k_at_the_top_are_no_ops() {
    // k at row 0 stays put; j at the last line stays put (clamped to the buffer).
    assert_eq!(row_col(r#"moveUp(initialState("a\nb"))"#), ints(0, 0));
    assert_eq!(row_col(r#"moveDown(moveDown(initialState("a\nb")))"#), ints(1, 0));
}

#[test]
fn moving_onto_a_shorter_line_reclamps_the_column() {
    // Cursor at col 2 on "aaa" (len 3); moving down to "b" (len 1) pulls col to 1.
    assert_eq!(
        row_col(r#"moveDown(Editor(..initialState("aaa\nb"), col: 2))"#),
        ints(1, 1)
    );
    // And a full-width line does not disturb the column: col 2 on "aaa" → "ccc".
    assert_eq!(
        row_col(r#"moveDown(Editor(..initialState("aaa\nccc"), col: 2))"#),
        ints(1, 2)
    );
}
