//! Snapshot tests for the stim editor FSM (`fs-image/stim/stim.st`).
//!
//! The FSM's *behaviour* is tested in Stitch, in `stim.st` itself — 57 native
//! `test` declarations beside the transitions they cover, run by the canon gate
//! (`canon.rs`) and, unlike anything here, runnable on the metal. See
//! `docs/stitch-testing-design.md`.
//!
//! What stays in Rust is what Stitch cannot yet express: `insta` snapshots. An
//! `expect` compares two values a test author wrote down; a snapshot records a
//! whole structure nobody wants to write down — the right tool for "the initial
//! state is *this*, in full" and for the ANSI byte soup `renderFrame` emits,
//! where the assertion's value is that it is exhaustive and unwritten. Native
//! snapshot assertions are deferred, not rejected: they need a file convention
//! and an accept workflow.

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
fn entering_insert_leaves_the_buffer_and_cursor_untouched() {
    // The mode flips; the snapshot is what says *only* the mode flipped. (That the
    // round trip through `enterNormal` is the identity is asserted natively.)
    insta::assert_debug_snapshot!(fsm(r#"enterInsert(initialState("ab"))"#));
}
