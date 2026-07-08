// stim — a minimal modal text editor, written as a Stitch program.
//
// The editor *is* this program: an immutable state value and a set of pure
// transitions over it. Rust natives provide only the primitives it can't
// express (string/list slicing) and, later, the host effects (raw key read,
// console write, fs write). The driver loop lives outside — this file is all
// logic, no I/O.
//
// Lives in its own ramfs folder: seeded from `fs-image/stim/` to `/stim/stim.st`
// (the seed recurses; the shell path-walks and `:load`s it). Also exercised at
// the host level through the interpreter (see `stitch/tests/stim_fsm.rs`).

use Str
use List

// The two modes. Normal navigates; Insert edits.
sum Mode = Normal | Insert

// The whole editor state — immutable. Every transition returns a fresh `Editor`
// via `..spread` update. `row`/`col` is a full cursor (0-based); unbound keys
// like `h`/`l` are just navigation we haven't wired, not missing state.
prod Editor(lines: List<Str>, row: Int, col: Int, mode: Mode)

// Build the initial state from a document's text: split into a line buffer at
// the origin in Normal mode. Splitting on "\n" makes the empty document one
// empty line (`[""]`), never zero lines — the cursor always has a line.
initialState(text) = Editor(
    lines: Str.split(text, "\n"),
    row: 0,
    col: 0,
    mode: Normal,
)

// The text of the line at `row`, or "" if out of range (never happens for a
// clamped cursor, but keeps every accessor total).
lineAt(state, row) = unwrapOr(List.at(state.lines, row), "")

// `col`, pulled left to the end of the line at `row` if that line is shorter —
// a vertical move must never leave the cursor past the end of its new line.
clampedCol(state, row) = {
    let lineLen = Str.length(lineAt(state, row))
    match { state.col > lineLen => lineLen  _ => state.col }
}

// `k` — up one line, clamped at the top; re-clamp the column onto the new line.
moveUp(state) = match {
    state.row <= 0 => state
    _ => {
        let row = state.row - 1
        Editor(..state, row: row, col: clampedCol(state, row))
    }
}

// `j` — down one line, clamped at the last line; re-clamp the column.
moveDown(state) = match {
    state.row >= count(state.lines) - 1 => state
    _ => {
        let row = state.row + 1
        Editor(..state, row: row, col: clampedCol(state, row))
    }
}

// `i` — enter Insert mode. Only the mode changes; buffer and cursor are untouched.
enterInsert(state) = Editor(..state, mode: Insert)

// `Esc` — return to Normal mode. The inverse of `enterInsert`; buffer/cursor kept.
enterNormal(state) = Editor(..state, mode: Normal)
