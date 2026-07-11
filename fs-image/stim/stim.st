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
// `Replace` is the one-shot operator-pending state for `r`: after `r`, the next key
// replaces the char under the cursor and returns to Normal. The gentlest form of
// cross-keystroke accumulation — a mode, exactly like Command already is.
sum Mode = Normal | Insert | Command | Replace

// What the driver must perform after a transition. `Save` carries the bytes to
// write back; `Redraw` asks for a screen repaint; `Noop` is nothing; `Quit` ends
// the session (the driver breaks its loop). (Named `Noop`, not `None`, so it
// doesn't clash with the prelude's `Maybe` `None`.)
// `Edit(name)` marks a buffer mutation, `name` being the telemetry span the driver
// opens for it (e.g. "stim.delete-char") — so the edit history is a trace on the
// wire. Pure navigation stays `Redraw`; a mutation that changed nothing also falls
// back to `Redraw` (see `editIfChanged`), so no-op keystrokes emit no span.
sum Effect = Save(Str) | Redraw | Noop | Quit | Edit(Str)

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
    state.col > lineLen => lineLen | state.col
}

// `k` — up one line, clamped at the top; re-clamp the column onto the new line.
moveUp(state) =
    state.row <= 0 => state
    | {
        let row = state.row - 1
        Editor(..state, row: row, col: clampedCol(state, row))
    }

// `j` — down one line, clamped at the last line; re-clamp the column.
moveDown(state) =
    state.row >= count(state.lines) - 1 => state
    | {
        let row = state.row + 1
        Editor(..state, row: row, col: clampedCol(state, row))
    }

// `h` — cursor left within the line, clamped at column 0.
moveLeft(state) = state.col > 0 => Editor(..state, col: state.col - 1) | state

// `l` — cursor right within the line, clamped at the line's end.
moveRight(state) =
    state.col < Str.length(lineAt(state, state.row)) => Editor(..state, col: state.col + 1) | state

// `0` — jump to the first column of the current line.
moveLineStart(state) = Editor(..state, col: 0)

// `$` — jump to the last character of the current line (`len − 1`), clamped at 0
// for an empty line. Lands *on* the last char (unlike `A`, which appends at `len`).
moveLineEnd(state) = {
    let lineLen = Str.length(lineAt(state, state.row))
    lineLen <= 0 => Editor(..state, col: 0) | Editor(..state, col: lineLen - 1)
}

// The column of the first non-space character at or after `i`, or the line length
// if the rest of the line is blank. Tail-recursive scan; v1 treats only " " as blank.
firstNonBlank(line, i) = match {
    i >= Str.length(line) => i
    Str.slice(line, i, i + 1) == " " => firstNonBlank(line, i + 1)
    _ => i
}

// `^` — jump to the first non-blank column. An all-blank line has no such column,
// so it clamps to col 0.
moveFirstNonBlank(state) = {
    let line = lineAt(state, state.row)
    let idx = firstNonBlank(line, 0)
    idx >= Str.length(line) => Editor(..state, col: 0) | Editor(..state, col: idx)
}

// `x` — delete the character under the cursor. No-op at end of line (nothing under
// the cursor); the cursor re-clamps onto the shortened line.
deleteChar(state) =
    state.col < Str.length(lineAt(state, state.row)) => {
        let line = lineAt(state, state.row)
        let edited = Str.slice(line, 0, state.col) + Str.slice(line, state.col + 1, Str.length(line))
        let moved = Editor(..state, lines: List.set(state.lines, state.row, edited))
        Editor(..moved, col: clampedCol(moved, state.row))
    }
    | state

// `X` — delete the character *before* the cursor (the mirror of `x`). No-op at
// column 0, where nothing precedes the cursor.
deleteBefore(state) =
    state.col > 0 => {
        let line = lineAt(state, state.row)
        let edited = Str.slice(line, 0, state.col - 1) + Str.slice(line, state.col, Str.length(line))
        Editor(..state, lines: List.set(state.lines, state.row, edited), col: state.col - 1)
    }
    | state

// `~` — toggle the case of the character under the cursor and advance one column.
// A char equal to its own lowercase is lowercase-or-non-alpha → uppercase it; else
// it's uppercase → lowercase it (non-alpha round-trips to itself). No-op at end of
// line, where there is no character under the cursor.
toggleCase(state) =
    state.col < Str.length(lineAt(state, state.row)) => {
        let line = lineAt(state, state.row)
        let ch = Str.slice(line, state.col, state.col + 1)
        let flipped = ch == Str.lower(ch) => Str.upper(ch) | Str.lower(ch)
        let edited = Str.slice(line, 0, state.col) + flipped + Str.slice(line, state.col + 1, Str.length(line))
        Editor(..state, lines: List.set(state.lines, state.row, edited), col: state.col + 1)
    }
    | state

// `J` — join the next line onto the current one with a single space at the seam,
// stripping the next line's leading blanks. The cursor lands at the join (the end
// of the original line). No-op on the last line, where there is nothing to join.
joinLines(state) =
    state.row < count(state.lines) - 1 => {
        let cur = lineAt(state, state.row)
        let next = lineAt(state, state.row + 1)
        let nextBody = Str.slice(next, firstNonBlank(next, 0), Str.length(next))
        let merged = List.set(state.lines, state.row, cur + " " + nextBody)
        Editor(..state, lines: List.removeAt(merged, state.row + 1), col: Str.length(cur))
    }
    | state

// `D` — delete from the cursor to the end of the line, keeping `[0, col)`. The
// cursor re-clamps onto the shortened line.
deleteToEol(state) = {
    let line = lineAt(state, state.row)
    let moved = Editor(..state, lines: List.set(state.lines, state.row, Str.slice(line, 0, state.col)))
    Editor(..moved, col: clampedCol(moved, state.row))
}

// `C` — change to end of line: `D` then enter Insert.
changeToEol(state) = enterInsert(deleteToEol(state))

// `s` — substitute: delete the char under the cursor, then enter Insert.
substitute(state) = enterInsert(deleteChar(state))

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

// `a` — append: advance the cursor one column (clamped at the line's end, `len`)
// and enter Insert, so typing lands *after* the char that was under the cursor.
appendAfter(state) = {
    let lineLen = Str.length(lineAt(state, state.row))
    let col = state.col < lineLen => state.col + 1 | lineLen
    Editor(..state, col: col, mode: Insert)
}

// `A` — append at end-of-line: jump to `len` and enter Insert.
appendEnd(state) = Editor(..state, col: Str.length(lineAt(state, state.row)), mode: Insert)

// `I` — insert at the first non-blank column, then enter Insert.
insertHead(state) = enterInsert(moveFirstNonBlank(state))

// `O` — open a fresh empty line *above* the cursor and enter Insert there (the
// mirror of `o`). Inserting at `row` pushes the current line and its successors down.
openAbove(state) = Editor(
    ..state,
    lines: List.insert(state.lines, state.row, ""),
    col: 0,
    mode: Insert,
)

// `Esc` — return to Normal mode. The inverse of `enterInsert`; buffer/cursor kept.
enterNormal(state) = Editor(..state, mode: Normal)

// `r` — enter Replace mode: the next key becomes the replacement char.
enterReplace(state) = Editor(..state, mode: Replace)

// Replace the character under the cursor with `ch` and return to Normal, keeping the
// cursor on the replaced char. No-op at end of line (nothing under the cursor), but
// still returns to Normal.
replaceChar(state, ch) =
    state.col < Str.length(lineAt(state, state.row)) => {
        let line = lineAt(state, state.row)
        let edited = Str.slice(line, 0, state.col) + ch + Str.slice(line, state.col + 1, Str.length(line))
        Editor(..state, lines: List.set(state.lines, state.row, edited), mode: Normal)
    }
    | Editor(..state, mode: Normal)

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

// A buffer mutation, tagged with its telemetry span name.
edit(state, name) = Step(state: state, effect: Edit(name))

// Emit an `Edit(name)` iff the transition actually changed the state; otherwise a
// bare `Redraw`. Keeps the edit trace honest — a clamped no-op (delete at a line
// edge, replace off the end) spans nothing.
editIfChanged(old, new, name) = old == new => redraw(new) | edit(new, name)

// A key is a printable char (vs. a symbolic name) iff it is exactly one char.
isPrintable(key) = Str.length(key) == 1

enterCommand(state) = Editor(..state, mode: Command)

// Normal mode: navigation, small edits, mode entry. Unbound keys are Noops.
stepNormal(state, key) = match {
    key == "j" => redraw(moveDown(state))
    key == "k" => redraw(moveUp(state))
    key == "h" => redraw(moveLeft(state))
    key == "l" => redraw(moveRight(state))
    key == "0" => redraw(moveLineStart(state))
    key == "$" => redraw(moveLineEnd(state))
    key == "^" => redraw(moveFirstNonBlank(state))
    key == "x" => editIfChanged(state, deleteChar(state), "stim.delete-char")
    key == "X" => editIfChanged(state, deleteBefore(state), "stim.delete-char")
    key == "~" => editIfChanged(state, toggleCase(state), "stim.toggle-case")
    key == "J" => editIfChanged(state, joinLines(state), "stim.join")
    key == "D" => editIfChanged(state, deleteToEol(state), "stim.delete-to-eol")
    key == "C" => edit(changeToEol(state), "stim.change-to-eol")
    key == "s" => edit(substitute(state), "stim.substitute")
    key == "r" => redraw(enterReplace(state))
    key == "o" => edit(openLine(state), "stim.open-line")
    key == "i" => redraw(enterInsert(state))
    key == "a" => redraw(appendAfter(state))
    key == "A" => redraw(appendEnd(state))
    key == "I" => redraw(insertHead(state))
    key == "O" => edit(openAbove(state), "stim.open-line")
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

// Replace mode (`r`): the next printable replaces the char under the cursor and
// returns to Normal; Esc (or any other key) cancels back to Normal with no edit.
// The change is measured against a Normal-mode baseline so a no-op replace (at end
// of line) spans nothing even though the mode transition itself changed the state.
stepReplace(state, key) = match {
    key == "Esc" => redraw(enterNormal(state))
    isPrintable(key) => editIfChanged(enterNormal(state), replaceChar(state, key), "stim.replace")
    _ => redraw(enterNormal(state))
}

step(state, key) = match state.mode {
    Normal => stepNormal(state, key)
    Insert => stepInsert(state, key)
    Command => stepCommand(state, key)
    Replace => stepReplace(state, key)
}
