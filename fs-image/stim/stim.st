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

// The modes. Normal navigates; Insert edits; Command reads a `:`-command (v1's
// only command is `:w`).
sum Mode = Normal | Insert | Command

// What the driver must perform after a transition. `Save` carries the bytes to
// write back; `Redraw` asks for a screen repaint; `Noop` is nothing; `Quit` ends
// the session (the driver breaks its loop). (Named `Noop`, not `None`, so it
// doesn't clash with the prelude's `Maybe` `None`.)
sum Effect = Save(Str) | Redraw | Noop | Quit

// The whole editor state — immutable. Every transition returns a fresh `Editor`
// via `..spread` update. `row`/`col` is a full cursor (0-based); unbound keys
// like `h`/`l` are just navigation we haven't wired, not missing state.
prod Editor(lines: List<Str>, row: Int, col: Int, mode: Mode)

// A transition's result for the driver: the next state plus the effect to run.
// The pure `step` returns this; the driver performs `effect` and loops on `state`.
prod Step(state: Editor, effect: Effect)

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

// `h` — cursor left within the line, clamped at column 0.
moveLeft(state) = match {
    state.col > 0 => Editor(..state, col: state.col - 1)
    _ => state
}

// `l` — cursor right within the line, clamped at the line's end.
moveRight(state) = match {
    state.col < Str.length(lineAt(state, state.row)) => Editor(..state, col: state.col + 1)
    _ => state
}

// `x` — delete the character under the cursor. No-op at end of line (nothing under
// the cursor); the cursor re-clamps onto the shortened line.
deleteChar(state) = match {
    state.col < Str.length(lineAt(state, state.row)) => {
        let line = lineAt(state, state.row)
        let edited = Str.slice(line, 0, state.col) + Str.slice(line, state.col + 1, Str.length(line))
        let moved = Editor(..state, lines: List.set(state.lines, state.row, edited))
        Editor(..moved, col: clampedCol(moved, state.row))
    }
    _ => state
}

// `o` — open a fresh empty line below the cursor and enter Insert mode there.
openLine(state) = Editor(
    ..state,
    lines: List.insert(state.lines, state.row + 1, ""),
    row: state.row + 1,
    col: 0,
    mode: Insert,
)

// `i` — enter Insert mode. Only the mode changes; buffer and cursor are untouched.
enterInsert(state) = Editor(..state, mode: Insert)

// `Esc` — return to Normal mode. The inverse of `enterInsert`; buffer/cursor kept.
enterNormal(state) = Editor(..state, mode: Normal)

// Insert the printable `ch` at the cursor: split the current line at `col`, drop
// `ch` between the halves, write the line back, and advance the cursor past it.
insertChar(state, ch) = {
    let line = lineAt(state, state.row)
    let before = Str.slice(line, 0, state.col)
    let after = Str.slice(line, state.col, Str.length(line))
    Editor(
        ..state,
        lines: List.set(state.lines, state.row, before + ch + after),
        col: state.col + Str.length(ch),
    )
}

// `Backspace` — three cases, checked in order:
//   col > 0            : delete the char just before the cursor (col−1).
//   col == 0, row > 0  : join this line onto the end of the previous one; the
//                        cursor lands at the seam (end of the old previous line).
//   col == 0, row == 0 : nothing before the cursor — a no-op.
backspace(state) = match {
    state.col > 0 => {
        let line = lineAt(state, state.row)
        let before = Str.slice(line, 0, state.col - 1)
        let after = Str.slice(line, state.col, Str.length(line))
        Editor(
            ..state,
            lines: List.set(state.lines, state.row, before + after),
            col: state.col - 1,
        )
    }
    state.row > 0 => {
        let prev = lineAt(state, state.row - 1)
        let cur = lineAt(state, state.row)
        let merged = List.set(state.lines, state.row - 1, prev + cur)
        Editor(
            ..state,
            lines: List.removeAt(merged, state.row),
            row: state.row - 1,
            col: Str.length(prev),
        )
    }
    _ => state
}

// `Enter` — split the current line at the cursor: the head `[0,col)` stays on this
// line, the tail `[col,len)` becomes a new line below, and the cursor moves to the
// start of that new line. The inverse of `backspace`'s line-join.
splitLine(state) = {
    let line = lineAt(state, state.row)
    let before = Str.slice(line, 0, state.col)
    let after = Str.slice(line, state.col, Str.length(line))
    let head = List.set(state.lines, state.row, before)
    Editor(
        ..state,
        lines: List.insert(head, state.row + 1, after),
        row: state.row + 1,
        col: 0,
    )
}

// The buffer serialized for saving: the lines joined back with newlines — the
// inverse of `initialState`'s split.
serialize(state) = Str.join(state.lines, "\n")

// `:w` — a `Save` effect carrying the whole serialized buffer. The state is
// unchanged (saving doesn't edit).
save(state) = Step(state: state, effect: Save(serialize(state)))

// Render a full-screen frame: clear + home (`\e[2J\e[H`), the buffer's lines
// (CRLF-separated so each starts in column 0 on a raw terminal), then an absolute
// cursor move to the 1-based `(row, col)`. v1 clears and redraws every keystroke —
// no diffing.
renderFrame(state) = {
    let body = Str.join(state.lines, "\r\n")
    "\e[2J\e[H{body}\e[{state.row + 1};{state.col + 1}H"
}

// --- The dispatch (`step`) ---------------------------------------------------
//
// `step(state, key) -> Step{state, effect}` routes a key by the current mode to
// the right transition. A `key` is either a single printable character (its own
// text, e.g. "a" or ":") or a symbolic name the driver maps raw bytes to:
// "Esc", "Enter", "Backspace". Every handled key redraws; save emits `Save`;
// unbound keys are a `Noop`.

redraw(state) = Step(state: state, effect: Redraw)
noop(state) = Step(state: state, effect: Noop)
quit(state) = Step(state: state, effect: Quit)

// A key is a printable char (vs. a symbolic name) iff it is exactly one char.
isPrintable(key) = Str.length(key) == 1

enterCommand(state) = Editor(..state, mode: Command)

// Normal mode: navigation, small edits, mode entry. Unbound keys are Noops.
stepNormal(state, key) = match {
    key == "j" => redraw(moveDown(state))
    key == "k" => redraw(moveUp(state))
    key == "h" => redraw(moveLeft(state))
    key == "l" => redraw(moveRight(state))
    key == "x" => redraw(deleteChar(state))
    key == "o" => redraw(openLine(state))
    key == "i" => redraw(enterInsert(state))
    key == ":" => redraw(enterCommand(state))
    _ => noop(state)
}

// Insert mode: printables edit the buffer; the named keys leave/split/delete.
stepInsert(state, key) = match {
    key == "Esc" => redraw(enterNormal(state))
    key == "Enter" => redraw(splitLine(state))
    key == "Backspace" => redraw(backspace(state))
    isPrintable(key) => redraw(insertChar(state, key))
    _ => noop(state)
}

// Command mode (`:`): `w` saves, `q` quits — both directly, no command-line
// accumulation. Each returns to Normal first; any other key cancels to Normal.
stepCommand(state, key) = match {
    key == "w" => save(enterNormal(state))
    key == "q" => quit(enterNormal(state))
    _ => redraw(enterNormal(state))
}

step(state, key) = match state.mode {
    Normal => stepNormal(state, key)
    Insert => stepInsert(state, key)
    Command => stepCommand(state, key)
}
