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

/// The effect tag ("Save"/"Redraw"/"Noop"/"Quit") of `step(setup, key)`.
fn step_effect(setup: &str, key: &str) -> Value {
    fsm(&format!(
        r#"{{ let st = step({setup}, "{key}")  match st.effect {{ Save(_) => "Save"  Redraw => "Redraw"  Noop => "Noop"  Quit => "Quit"  Edit(_) => "Edit" }} }}"#
    ))
}

/// The resulting mode name of `step(setup, key)`.
fn step_mode(setup: &str, key: &str) -> Value {
    fsm(&format!(
        r#"{{ let st = step({setup}, "{key}")  match st.state.mode {{ Normal => "Normal"  Insert => "Insert"  Command => "Command"  Replace => "Replace" }} }}"#
    ))
}

fn s(text: &str) -> Value {
    Value::Str(text.into())
}

/// The name of the pending operator after `step(setup, key)` — "None" when not
/// operator-pending.
fn pending_op(setup: &str, key: &str) -> Value {
    fsm(&format!(
        r#"{{ let st = step({setup}, "{key}")  match st.state.pending.op {{ OpNone => "None"  OpDelete => "Delete"  OpChange => "Change"  OpYank => "Yank" }} }}"#
    ))
}

/// The `[row, col]` of `motionTarget(setup, key)`, or `[0, 0]` if the key is not a
/// motion. Binding `rc` keeps the trailing `[…]` off the match (maximal munch).
fn motion_rowcol(setup: &str, key: &str) -> Value {
    fsm(&format!(
        r#"{{ match motionTarget({setup}, "{key}") {{ Some(t) => {{ let rc = [t.row, t.col]  rc }}  None => {{ let z = [0, 0]  z }} }} }}"#
    ))
}

/// The wiseness name of `motionTarget(setup, key)` — "Charwise"/"Linewise", or
/// "None" if the key is not a motion.
fn motion_wise(setup: &str, key: &str) -> Value {
    fsm(&format!(
        r#"{{ match motionTarget({setup}, "{key}") {{ Some(t) => (match t.wise {{ Charwise => "Charwise"  Linewise => "Linewise" }})  None => "None" }} }}"#
    ))
}

/// The name of the pending operator of the `Editor` that `setup` evaluates to.
fn pending_op_of(setup: &str) -> Value {
    fsm(&format!(
        r#"{{ let s = {setup}  match s.pending.op {{ OpNone => "None"  OpDelete => "Delete"  OpChange => "Change"  OpYank => "Yank" }} }}"#
    ))
}

#[test]
fn step_dispatches_normal_mode_keys() {
    let start = r#"initialState("a\nb")"#;
    // j moves down (Redraw); i enters Insert; : enters Command.
    assert_eq!(step_effect(start, "j"), s("Redraw"));
    assert_eq!(row_col(&format!(r#"step({start}, "j").state"#)), ints(1, 0));
    assert_eq!(step_mode(start, "i"), s("Insert"));
    assert_eq!(step_mode(start, ":"), s("Command"));
    // An unbound key (`z`) is a Noop — nothing changes.
    assert_eq!(step_effect(start, "z"), s("Noop"));
    assert_eq!(fsm(&format!(r#"step({start}, "z").state"#)), fsm(start));
}

#[test]
fn h_and_l_move_the_cursor_within_the_line() {
    // "abc" cursor at col 1: `l` → col 2, `h` → col 0.
    let mid = r#"Editor(..initialState("abc"), col: 1)"#;
    assert_eq!(row_col(&format!(r#"step({mid}, "l").state"#)), ints(0, 2));
    assert_eq!(row_col(&format!(r#"step({mid}, "h").state"#)), ints(0, 0));
    // `h` at column 0 and `l` at the line's end are both no-ops (clamped).
    assert_eq!(row_col(r#"step(initialState("abc"), "h").state"#), ints(0, 0));
    assert_eq!(row_col(r#"step(Editor(..initialState("abc"), col: 3), "l").state"#), ints(0, 3));
    // They're handled keys, so they redraw (even the clamped no-op).
    assert_eq!(step_effect(mid, "h"), s("Redraw"));
}

#[test]
fn x_deletes_the_character_under_the_cursor() {
    // "abc" col 1 → delete 'b' → "ac", col 1.
    assert_eq!(
        lines_col(r#"step(Editor(..initialState("abc"), col: 1), "x").state"#),
        line_and_col("ac", 1)
    );
    // At end of line (col == len) there's nothing under the cursor → no-op.
    assert_eq!(
        lines_col(r#"step(Editor(..initialState("abc"), col: 3), "x").state"#),
        line_and_col("abc", 3)
    );
    // Deleting the last char re-clamps the cursor onto the shortened line.
    assert_eq!(
        lines_col(r#"step(Editor(..initialState("ab"), col: 1), "x").state"#),
        line_and_col("a", 1)
    );
}

#[test]
fn capital_x_deletes_the_char_before_the_cursor_as_an_observable_edit() {
    // "abc" col 2 → `X` deletes 'b' → "ac", col 1 (the mirror of `x`, which deletes
    // the char *under* the cursor).
    assert_eq!(
        lines_col(r#"step(Editor(..initialState("abc"), col: 2), "X").state"#),
        line_and_col("ac", 1)
    );
    // At col 0 there is nothing before the cursor → the buffer is untouched.
    assert_eq!(
        lines_col(r#"step(Editor(..initialState("abc"), col: 0), "X").state"#),
        line_and_col("abc", 0)
    );
    // A real deletion is an observable `Edit` effect — the driver spans it — not a
    // bare `Redraw`.
    assert_eq!(step_effect(r#"Editor(..initialState("abc"), col: 2)"#, "X"), s("Edit"));
    // A no-op `X` (col 0) changed nothing, so it falls back to `Redraw` and emits no
    // span — telemetry reflects real edits only.
    assert_eq!(step_effect(r#"Editor(..initialState("abc"), col: 0)"#, "X"), s("Redraw"));
}

#[test]
fn o_opens_a_line_below_and_enters_insert() {
    // "ab" → `o` → an empty line below, cursor there, in Insert mode.
    assert_eq!(
        lines_row_col(r#"step(initialState("ab"), "o").state"#),
        buffer(&["ab", ""], 1, 0)
    );
    assert_eq!(step_mode(r#"initialState("ab")"#, "o"), s("Insert"));
}

#[test]
fn a_capital_a_and_capital_i_position_the_cursor_and_enter_insert() {
    // `a` appends: the cursor advances one column and enters Insert (insert *after*
    // the char under the cursor).
    assert_eq!(row_col(r#"step(initialState("ab"), "a").state"#), ints(0, 1));
    assert_eq!(step_mode(r#"initialState("ab")"#, "a"), s("Insert"));
    // `a` at end of line clamps at `len` — it can't advance past the buffer.
    assert_eq!(row_col(r#"step(Editor(..initialState("ab"), col: 2), "a").state"#), ints(0, 2));
    // `A` jumps to end-of-line (col == len) and enters Insert.
    assert_eq!(row_col(r#"step(initialState("ab"), "A").state"#), ints(0, 2));
    // `I` jumps to the first non-blank column and enters Insert.
    assert_eq!(row_col(r#"step(Editor(..initialState("  ab"), col: 3), "I").state"#), ints(0, 2));
    // Each only moves the cursor + switches mode — no buffer edit → Redraw.
    assert_eq!(step_effect(r#"initialState("ab")"#, "a"), s("Redraw"));
    assert_eq!(step_effect(r#"initialState("ab")"#, "A"), s("Redraw"));
    assert_eq!(step_effect(r#"Editor(..initialState("  ab"), col: 3)"#, "I"), s("Redraw"));
}

#[test]
fn capital_o_opens_a_line_above_and_enters_insert() {
    // `O` on row 1 opens a fresh empty line *above* it (the mirror of `o` below),
    // cursor there, in Insert.
    assert_eq!(
        lines_row_col(r#"step(Editor(..initialState("x\ny"), row: 1), "O").state"#),
        buffer(&["x", "", "y"], 1, 0)
    );
    assert_eq!(step_mode(r#"initialState("ab")"#, "O"), s("Insert"));
    // Opening a line mutates the buffer → an observable Edit.
    assert_eq!(step_effect(r#"initialState("ab")"#, "O"), s("Edit"));
}

#[test]
fn tilde_toggles_the_case_under_the_cursor_and_advances() {
    // "aBc" col 0: `~` flips 'a'→'A' → "ABc" and advances to col 1.
    assert_eq!(
        lines_col(r#"step(initialState("aBc"), "~").state"#),
        line_and_col("ABc", 1)
    );
    // On an uppercase char it flips the other way: col 1 'B'→'b' → "abc", col 2.
    assert_eq!(
        lines_col(r#"step(Editor(..initialState("aBc"), col: 1), "~").state"#),
        line_and_col("abc", 2)
    );
    // At end of line (nothing under the cursor) it's a no-op → Redraw, no span.
    assert_eq!(step_effect(r#"Editor(..initialState("aBc"), col: 3)"#, "~"), s("Redraw"));
    // A real toggle is an observable Edit.
    assert_eq!(step_effect(r#"initialState("aBc")"#, "~"), s("Edit"));
}

#[test]
fn capital_j_joins_the_next_line_onto_the_current_one() {
    // "ab" / "cd" → `J` → "ab cd" (one space at the seam), cursor at the join (col 2),
    // and the second line is gone.
    assert_eq!(
        lines_row_col(r#"step(initialState("ab\ncd"), "J").state"#),
        buffer(&["ab cd"], 0, 2)
    );
    // The next line's leading blanks are stripped before joining (vim's collapse).
    assert_eq!(
        lines_row_col(r#"step(initialState("ab\n   cd"), "J").state"#),
        buffer(&["ab cd"], 0, 2)
    );
    // On the last line there is nothing to join → a no-op Redraw.
    assert_eq!(step_effect(r#"Editor(..initialState("ab\ncd"), row: 1)"#, "J"), s("Redraw"));
    // A real join is an observable Edit.
    assert_eq!(step_effect(r#"initialState("ab\ncd")"#, "J"), s("Edit"));
}

#[test]
fn capital_d_deletes_from_the_cursor_to_the_end_of_line() {
    // "abcd" col 1 → `D` drops "bcd", leaving "a".
    assert_eq!(
        lines_col(r#"step(Editor(..initialState("abcd"), col: 1), "D").state"#),
        line_and_col("a", 1)
    );
    // At end of line there is nothing to delete → a no-op Redraw.
    assert_eq!(step_effect(r#"Editor(..initialState("ab"), col: 2)"#, "D"), s("Redraw"));
    assert_eq!(step_effect(r#"Editor(..initialState("abcd"), col: 1)"#, "D"), s("Edit"));
    // `D` stays in Normal mode.
    assert_eq!(step_mode(r#"Editor(..initialState("abcd"), col: 1)"#, "D"), s("Normal"));
}

#[test]
fn capital_c_changes_to_end_of_line_and_enters_insert() {
    // `C` is `D` then Insert: "abcd" col 1 → "a", cursor at col 1, in Insert.
    assert_eq!(
        lines_col(r#"step(Editor(..initialState("abcd"), col: 1), "C").state"#),
        line_and_col("a", 1)
    );
    assert_eq!(step_mode(r#"Editor(..initialState("abcd"), col: 1)"#, "C"), s("Insert"));
    assert_eq!(step_effect(r#"Editor(..initialState("abcd"), col: 1)"#, "C"), s("Edit"));
}

#[test]
fn s_substitutes_the_char_under_the_cursor_and_enters_insert() {
    // `s` deletes the char under the cursor and enters Insert: "abc" col 1 → "ac",
    // col 1, Insert.
    assert_eq!(
        lines_col(r#"step(Editor(..initialState("abc"), col: 1), "s").state"#),
        line_and_col("ac", 1)
    );
    assert_eq!(step_mode(r#"Editor(..initialState("abc"), col: 1)"#, "s"), s("Insert"));
    assert_eq!(step_effect(r#"Editor(..initialState("abc"), col: 1)"#, "s"), s("Edit"));
}

#[test]
fn r_replaces_the_char_under_the_cursor_via_a_two_key_sequence() {
    // `r` enters Replace mode — a Redraw, no edit yet — the mini-accumulator waiting
    // for the replacement char.
    assert_eq!(step_mode(r#"initialState("abc")"#, "r"), s("Replace"));
    assert_eq!(step_effect(r#"initialState("abc")"#, "r"), s("Redraw"));
    // In Replace mode the next printable replaces the char under the cursor and
    // returns to Normal: "abc" col 1, then `Z` → "aZc", cursor stays at col 1.
    let pend = r#"Editor(..initialState("abc"), col: 1, mode: Replace)"#;
    assert_eq!(lines_col(&format!(r#"step({pend}, "Z").state"#)), line_and_col("aZc", 1));
    assert_eq!(step_mode(pend, "Z"), s("Normal"));
    assert_eq!(step_effect(pend, "Z"), s("Edit"));
    // Esc cancels Replace mode with no edit.
    assert_eq!(step_mode(pend, "Esc"), s("Normal"));
    assert_eq!(step_effect(pend, "Esc"), s("Redraw"));
    // `r` at end of line then a char is a no-op (nothing under the cursor) → Redraw,
    // and still returns to Normal.
    let at_end = r#"Editor(..initialState("abc"), col: 3, mode: Replace)"#;
    assert_eq!(step_effect(at_end, "Z"), s("Redraw"));
    assert_eq!(step_mode(at_end, "Z"), s("Normal"));
}

#[test]
fn step_dispatches_insert_mode_keys() {
    let ins = r#"Editor(..initialState("ac"), col: 1, mode: Insert)"#;
    // A printable inserts and advances; Esc→Normal; Enter splits; Backspace deletes.
    assert_eq!(step_effect(ins, "b"), s("Redraw"));
    assert_eq!(lines_col(&format!(r#"step({ins}, "b").state"#)), line_and_col("abc", 2));
    assert_eq!(step_mode(ins, "Esc"), s("Normal"));
    assert_eq!(step_mode(ins, "Enter"), s("Insert")); // split keeps Insert mode
    assert_eq!(lines_col(&format!(r#"step({ins}, "Backspace").state"#)), line_and_col("c", 0));
}

#[test]
fn step_colon_w_saves_the_buffer_and_returns_to_normal() {
    let start = r#"initialState("hi\nthere")"#;
    // The full `:w`: `:` enters Command, then `w` yields a Save effect.
    let cmd = format!(r#"step({start}, ":").state"#);
    assert_eq!(step_effect(&cmd, "w"), s("Save"));
    // The Save carries the serialized buffer, and we return to Normal.
    assert_eq!(
        fsm(&format!(r#"{{ let st = step({cmd}, "w")  match st.effect {{ Save(t) => t  _ => "?" }} }}"#)),
        s("hi\nthere")
    );
    assert_eq!(step_mode(&cmd, "w"), s("Normal"));
    // `:q` quits — a Quit effect (the driver breaks its loop on it) — and returns
    // to Normal.
    assert_eq!(step_effect(&cmd, "q"), s("Quit"));
    assert_eq!(step_mode(&cmd, "q"), s("Normal"));
    // Any *other* command key cancels back to Normal (a harmless Redraw, no save).
    assert_eq!(step_effect(&cmd, "z"), s("Redraw"));
    assert_eq!(step_mode(&cmd, "z"), s("Normal"));
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
fn operators_enter_operator_pending_within_normal_mode() {
    // A fresh state is not operator-pending.
    assert_eq!(pending_op_of(r#"initialState("abc")"#), s("None"));
    // `d` does not act yet — it stays in Normal mode with a pending Delete operator,
    // awaiting a motion. A Redraw, no edit.
    assert_eq!(step_mode(r#"initialState("abc")"#, "d"), s("Normal"));
    assert_eq!(step_effect(r#"initialState("abc")"#, "d"), s("Redraw"));
    assert_eq!(pending_op(r#"initialState("abc")"#, "d"), s("Delete"));
    // `c` and `y` likewise set their operator and stay in Normal.
    assert_eq!(pending_op(r#"initialState("abc")"#, "c"), s("Change"));
    assert_eq!(pending_op(r#"initialState("abc")"#, "y"), s("Yank"));
}

#[test]
fn motion_target_carries_position_and_wiseness() {
    // `$` yields a charwise target at the last char (col 4 of "hello") — motionTarget
    // is the single definition the bare move and the operator both consume.
    assert_eq!(motion_rowcol(r#"initialState("hello")"#, "$"), ints(0, 4));
    assert_eq!(motion_wise(r#"initialState("hello")"#, "$"), s("Charwise"));
    // `j` yields a linewise target on the next row, carrying the column.
    assert_eq!(motion_rowcol(r#"Editor(..initialState("ab\ncd"), col: 1)"#, "j"), ints(1, 1));
    assert_eq!(motion_wise(r#"Editor(..initialState("ab\ncd"), col: 1)"#, "j"), s("Linewise"));
    // A non-motion key has no target.
    assert_eq!(motion_wise(r#"initialState("hello")"#, "z"), s("None"));
}

#[test]
fn d_plus_a_charwise_motion_deletes_the_intra_line_range() {
    // Drive operator-pending directly: `pending.op = OpDelete`, then a charwise motion.
    // d$ deletes from the cursor through end-of-line (inclusive) — same result as D.
    assert_eq!(
        lines_col(r#"step(Editor(..initialState("abcd"), col: 1, pending: Pending(op: OpDelete)), "$").state"#),
        line_and_col("a", 1)
    );
    // dl deletes the char under the cursor (exclusive, forward) — like x.
    assert_eq!(
        lines_col(r#"step(Editor(..initialState("abcd"), col: 1, pending: Pending(op: OpDelete)), "l").state"#),
        line_and_col("acd", 1)
    );
    // dh deletes the char before the cursor (backward) — like X, cursor moves back.
    assert_eq!(
        lines_col(r#"step(Editor(..initialState("abcd"), col: 1, pending: Pending(op: OpDelete)), "h").state"#),
        line_and_col("bcd", 0)
    );
    // d0 deletes from line-start up to the cursor (exclusive of the char under it).
    assert_eq!(
        lines_col(r#"step(Editor(..initialState("abcd"), col: 2, pending: Pending(op: OpDelete)), "0").state"#),
        line_and_col("cd", 0)
    );
    // The operator clears after applying, and a real delete is an observable Edit.
    assert_eq!(
        pending_op_of(r#"step(Editor(..initialState("abcd"), col: 1, pending: Pending(op: OpDelete)), "l").state"#),
        s("None")
    );
    assert_eq!(
        step_effect(r#"Editor(..initialState("abcd"), col: 1, pending: Pending(op: OpDelete))"#, "l"),
        s("Edit")
    );
    // dl at end of line deletes nothing (range past the last char) → Redraw, and still
    // clears the pending operator.
    assert_eq!(
        step_effect(r#"Editor(..initialState("ab"), col: 2, pending: Pending(op: OpDelete))"#, "l"),
        s("Redraw")
    );
    assert_eq!(
        pending_op_of(r#"step(Editor(..initialState("ab"), col: 2, pending: Pending(op: OpDelete)), "l").state"#),
        s("None")
    );
}

#[test]
fn zero_and_dollar_jump_to_the_line_ends() {
    // "hello" with the cursor mid-line: `0` → column 0, `$` → the last character
    // (col 4). `$` lands *on* the last char (len−1), unlike `A` which appends at len.
    let mid = r#"Editor(..initialState("hello"), col: 2)"#;
    assert_eq!(row_col(&format!(r#"step({mid}, "0").state"#)), ints(0, 0));
    assert_eq!(row_col(&format!(r#"step({mid}, "$").state"#)), ints(0, 4));
    // On an empty line `$` clamps to col 0 (max(len−1, 0)), never −1.
    assert_eq!(row_col(r#"step(initialState(""), "$").state"#), ints(0, 0));
    // Pure motions — a handled key that only moves the cursor is a Redraw, not an
    // Edit (nothing in the buffer changed).
    assert_eq!(step_effect(mid, "0"), s("Redraw"));
    assert_eq!(step_effect(mid, "$"), s("Redraw"));
}

#[test]
fn caret_jumps_to_the_first_non_blank_column() {
    // "  hi" → `^` lands on col 2 (the 'h'), skipping the two leading spaces.
    assert_eq!(row_col(r#"step(Editor(..initialState("  hi"), col: 3), "^").state"#), ints(0, 2));
    // A line with no content (all spaces) clamps `^` to col 0.
    assert_eq!(row_col(r#"step(Editor(..initialState("   "), col: 2), "^").state"#), ints(0, 0));
    // No leading blanks → `^` is column 0, same as `0`.
    assert_eq!(row_col(r#"step(Editor(..initialState("hi"), col: 1), "^").state"#), ints(0, 0));
    assert_eq!(step_effect(r#"Editor(..initialState("  hi"), col: 3)"#, "^"), s("Redraw"));
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
fn render_frame_clears_draws_lines_and_positions_the_cursor() {
    // A two-line buffer at the origin: clear+home, the lines (CRLF-separated), then
    // a cursor move to (1,1) — ANSI is 1-based.
    insta::assert_debug_snapshot!(fsm(r#"renderFrame(initialState("ab\ncd"))"#));
}

#[test]
fn render_frame_moves_the_cursor_to_the_1_based_position() {
    // Cursor at row 1, col 3 → the trailing move is ESC[2;4H (both +1).
    insta::assert_debug_snapshot!(
        fsm(r#"renderFrame(Editor(..initialState("hello\nworld"), row: 1, col: 3))"#)
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
