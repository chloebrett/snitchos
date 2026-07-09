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

/// Project `[lines, col]` from the `Editor` that `setup` evaluates to — for
/// edit transitions, where the line buffer and the column both change.
fn lines_col(setup: &str) -> Value {
    fsm(&format!("{{ let s = {setup}  let out = [s.lines, s.col]  out }}"))
}

/// Project `[lines, row, col]` — for transitions that move the cursor across
/// lines (a line-join changes all three).
fn lines_row_col(setup: &str) -> Value {
    fsm(&format!("{{ let s = {setup}  let out = [s.lines, s.row, s.col]  out }}"))
}

/// The expected `[lines, row, col]` for a multi-line buffer.
fn buffer(lines: &[&str], row: i64, col: i64) -> Value {
    let ls = lines.iter().map(|l| Value::Str((*l).into())).collect::<Vec<_>>();
    Value::List(vec![Value::List(ls.into()), Value::Int(row), Value::Int(col)].into())
}

/// The expected `[lines, col]` for a single-line buffer holding `text`.
fn line_and_col(text: &str, col: i64) -> Value {
    Value::List(
        vec![
            Value::List(vec![Value::Str(text.into())].into()),
            Value::Int(col),
        ]
        .into(),
    )
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
fn insert_a_printable_char_splits_the_line_and_advances_the_column() {
    // Mid-line: "ac" with the cursor at col 1, insert 'b' → "abc", col 2.
    assert_eq!(
        lines_col(r#"insertChar(Editor(..initialState("ac"), col: 1), "b")"#),
        line_and_col("abc", 2)
    );
    // End of line: "ab" at col 2 (the end), insert 'c' → "abc", col 3.
    assert_eq!(
        lines_col(r#"insertChar(Editor(..initialState("ab"), col: 2), "c")"#),
        line_and_col("abc", 3)
    );
    // Start of line: "bc" at col 0, insert 'a' → "abc", col 1.
    assert_eq!(
        lines_col(r#"insertChar(initialState("bc"), "a")"#),
        line_and_col("abc", 1)
    );
}

#[test]
fn insert_edits_only_the_cursor_row_and_leaves_other_lines() {
    // Two lines, cursor on row 1 col 1: inserting 'Z' edits only "yy"→"yZy" and
    // leaves "xx" untouched — the edit lands on the cursor's row, not row 0.
    let expected = Value::List(
        vec![
            Value::List(vec![Value::Str("xx".into()), Value::Str("yZy".into())].into()),
            Value::Int(2),
        ]
        .into(),
    );
    assert_eq!(
        lines_col(r#"insertChar(Editor(..initialState("xx\nyy"), row: 1, col: 1), "Z")"#),
        expected
    );
}

#[test]
fn backspace_deletes_the_char_before_the_cursor() {
    // "abc" with the cursor at col 2 → deletes 'b' → "ac", col 1.
    assert_eq!(
        lines_col(r#"backspace(Editor(..initialState("abc"), col: 2))"#),
        line_and_col("ac", 1)
    );
}

#[test]
fn backspace_at_col_0_joins_onto_the_previous_line() {
    // Three lines, cursor at row 2 col 0: joins "c" onto "b" → ["a","bc"], and the
    // cursor lands at the join (row 1, col = len "b" = 1). Proves the join targets
    // row-1 / removeAt(row), not hardcoded indices.
    assert_eq!(
        lines_row_col(r#"backspace(Editor(..initialState("a\nb\nc"), row: 2, col: 0))"#),
        buffer(&["a", "bc"], 1, 1)
    );
}

#[test]
fn backspace_at_the_top_left_is_a_no_op() {
    // Row 0, col 0: nothing before the cursor — a round-trip identity no-op.
    assert_eq!(
        fsm(r#"backspace(initialState("ab"))"#),
        fsm(r#"initialState("ab")"#)
    );
}

#[test]
fn enter_splits_the_line_at_the_cursor_into_two() {
    // Middle: "abc" at col 1 → "a" / "bc", cursor to the new line (row 1, col 0).
    assert_eq!(
        lines_row_col(r#"splitLine(Editor(..initialState("abc"), col: 1))"#),
        buffer(&["a", "bc"], 1, 0)
    );
    // At end: col 3 → "abc" and a fresh empty line below.
    assert_eq!(
        lines_row_col(r#"splitLine(Editor(..initialState("abc"), col: 3))"#),
        buffer(&["abc", ""], 1, 0)
    );
    // At start: col 0 → an empty line above, content pushed down.
    assert_eq!(
        lines_row_col(r#"splitLine(initialState("abc"))"#),
        buffer(&["", "abc"], 1, 0)
    );
}

#[test]
fn enter_splits_only_the_cursor_line_and_keeps_the_rest() {
    // Three lines, split "abc" (row 1) at col 1 → x, a, bc, y; the split lands on
    // the cursor row and its neighbours are undisturbed.
    assert_eq!(
        lines_row_col(r#"splitLine(Editor(..initialState("x\nabc\ny"), row: 1, col: 1))"#),
        buffer(&["x", "a", "bc", "y"], 2, 0)
    );
}

#[test]
fn enter_then_backspace_is_the_identity() {
    // `splitLine` leaves the cursor at col 0 of the new line, exactly where
    // `backspace` joins back — so the two compose to the original state. A
    // round-trip that catches an off-by-one in *either* transition.
    assert_eq!(
        fsm(r#"backspace(splitLine(Editor(..initialState("abc"), col: 1)))"#),
        fsm(r#"Editor(..initialState("abc"), col: 1)"#)
    );
}

#[test]
fn serialize_joins_the_line_buffer_with_newlines() {
    // The inverse of `initialState`'s split: lines back to one newline-joined blob.
    assert_eq!(
        fsm(r#"serialize(initialState("a\nb\nc"))"#),
        Value::Str("a\nb\nc".into())
    );
    // A single empty line serializes to the empty string (round-trips "").
    assert_eq!(fsm(r#"serialize(initialState(""))"#), Value::Str("".into()));
}

#[test]
fn save_yields_a_save_effect_carrying_the_serialized_buffer() {
    // `:w` → Step{state, Save(text)} where text is the whole buffer joined by \n.
    assert_eq!(
        fsm(r#"{ let st = save(initialState("a\nb"))  match st.effect { Save(t) => t  _ => "?" } }"#),
        Value::Str("a\nb".into())
    );
    // Saving does not edit the buffer — the Step carries the state unchanged.
    assert_eq!(
        fsm(r#"{ let st = save(initialState("a\nb"))  st.state }"#),
        fsm(r#"initialState("a\nb")"#)
    );
}

#[test]
fn i_enters_insert_and_esc_returns_to_normal() {
    // `i` flips Normal→Insert; the snapshot shows the buffer/cursor untouched.
    insta::assert_debug_snapshot!(fsm(r#"enterInsert(initialState("ab"))"#));
    // `Esc` after `i` returns to *exactly* the starting state — a round-trip
    // identity proves the mode switch alone changed, nothing else.
    assert_eq!(
        fsm(r#"enterNormal(enterInsert(initialState("ab")))"#),
        fsm(r#"initialState("ab")"#)
    );
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
