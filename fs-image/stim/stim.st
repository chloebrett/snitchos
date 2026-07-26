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

// The operator awaiting a motion in operator-pending state. `OpNone` means "not
// operator-pending" — the top level of `stepNormal`.
sum Op = OpNone | OpDelete | OpChange | OpYank

// A motion's (and a register's) shape: charwise acts on a span within a line,
// linewise on whole lines.
sum Wise = Charwise | Linewise

// A text-object prefix awaiting its object char: `iw`/`aw`, `i"`/`a"`. `NoObj` = not
// mid-object. `Inner` = contents only; `Around` = contents + delimiters/whitespace.
sum ObjKind = NoObj | Inner | Around

// The operator-pending accumulator — the whole partial command: the pending operator,
// a count prefix, and a text-object prefix. A field on `Editor` (not a mode) —
// operator-pending stays *within* Normal mode (the cursor still shows). It resets as a
// unit via `clearPending`. (Register slot arrives in Phase 5.)
prod Pending(op: Op, count: Int, object: ObjKind)

// The unnamed register (the yank/paste clipboard). Carries its wiseness so paste
// knows linewise (open a line) vs charwise (insert inline). This is the soft form of
// the future clipboard-service entry — kept a growable value type so it can absorb
// provenance / label / caps later (see docs/clipboard-design.md).
prod Register(text: Str, wise: Wise)

// A motion's result: a target cursor position, the wiseness that decides (under an
// operator) span-within-a-line vs whole-lines, and `inclusive` — whether an operator
// range includes the target character. Only `$` is inclusive; `h l 0 ^` are exclusive.
prod Target(row: Int, col: Int, wise: Wise, inclusive: Bool)

// A cursor-independent region an operator acts on — the shape a text object produces
// (both ends set by the object, unlike a motion's `[cursor, target]`). Charwise = the
// half-open span `[startCol, endCol)` on `startRow`; linewise = whole rows
// `[startRow, endRow]`. v1 objects are all single-row charwise.
prod Range(startRow: Int, startCol: Int, endRow: Int, endCol: Int, wise: Wise)

// The whole editor state — immutable. Every transition returns a fresh `Editor`
// via `..spread` update. `row`/`col` is a full cursor (0-based); unbound keys
// like `h`/`l` are just navigation we haven't wired, not missing state.
prod Editor(lines: List<Str>, row: Int, col: Int, mode: Mode, pending: Pending, clipboard: Register)

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
    pending: Pending(op: OpNone, count: 0, object: NoObj),
    clipboard: Register(text: "", wise: Charwise),
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

// The column of the first non-space character at or after `i`, or the line length
// if the rest of the line is blank. Tail-recursive scan; v1 treats only " " as blank.
firstNonBlank(line, i) = match {
    i >= Str.length(line) => i
    Str.slice(line, i, i + 1) == " " => firstNonBlank(line, i + 1)
    _ => i
}

// The column `$` targets: the last character (`len − 1`), clamped to 0 on an empty
// line — lands *on* the last char (unlike `A`, which appends at `len`).
lineEndCol(state) = {
    let lineLen = Str.length(lineAt(state, state.row))
    lineLen <= 0 => 0 | lineLen - 1
}

// The column `^` (and `I`) target: the first non-blank column, or 0 if the line is
// all blank.
firstNonBlankCol(state) = {
    let line = lineAt(state, state.row)
    let idx = firstNonBlank(line, 0)
    idx >= Str.length(line) => 0 | idx
}

// Clamp a row index into the buffer `[0, count − 1]`.
clampRow(state, r) = r < 0 => 0 | (r > count(state.lines) - 1 => count(state.lines) - 1 | r)

// --- Word motions (whitespace-delimited "words"; v1 treats `w` like `W` — a run of
// non-space chars — so punctuation is not yet its own word). Within-line only;
// cross-line word motion is deferred. ---

// Forward: the first space index at/after `i` (or `len`) — the end of a non-space run.
scanWordEnd(line, i, len) = match {
    i >= len => i
    Str.slice(line, i, i + 1) == " " => i
    _ => scanWordEnd(line, i + 1, len)
}

// Forward: the first non-space index at/after `i` (or `len`) — skips a run of spaces.
scanSpaces(line, i, len) = match {
    i >= len => i
    Str.slice(line, i, i + 1) == " " => scanSpaces(line, i + 1, len)
    _ => i
}

// Backward: the first non-space index at/below `i` (or -1) — skips spaces leftward.
scanSpacesBack(line, i) = match {
    i < 0 => i
    Str.slice(line, i, i + 1) == " " => scanSpacesBack(line, i - 1)
    _ => i
}

// Backward: the start of the word containing `i` (walk left while the char to the
// left is non-space).
scanWordStartBack(line, i) = match {
    i <= 0 => 0
    Str.slice(line, i - 1, i) == " " => i
    _ => scanWordStartBack(line, i - 1)
}

// `w` — the next word start (or line end if none): skip the current non-space run,
// then the following spaces. Exclusive.
wordForwardCol(line, c) = scanSpaces(line, scanWordEnd(line, c, Str.length(line)), Str.length(line))

// `e` — the end of the current/next word (inclusive): skip spaces from `c + 1`, then
// take that word's last char. Stays put if no word lies ahead.
wordEndCol(line, c) = {
    let len = Str.length(line)
    let start = scanSpaces(line, c + 1, len)
    start >= len => c | scanWordEnd(line, start, len) - 1
}

// `b` — the previous word start (exclusive, backward): skip spaces left of the
// cursor, then walk to that word's start.
wordBackCol(line, c) = {
    let start = scanSpacesBack(line, c - 1)
    start < 0 => 0 | scanWordStartBack(line, start)
}

// The three word motions against one line, at the boundaries that distinguish
// them: `w` is exclusive (next word *start*), `e` is inclusive (this word's last
// char), `b` goes back to a start. "foo bar baz" puts word starts at 0/4/8 and
// word ends at 2/6/10, so an off-by-one in either scan shows up here.
test "word motions move by words within the line" {
    let line = initialState("foo bar baz")
    // Word motions are intra-line: the row must not move.
    expect step(line, "w").state.row == 0
    expect step(line, "w").state.col == 4
    expect step(Editor(..line, col: 4), "w").state.col == 8
    expect step(line, "e").state.col == 2
    expect step(Editor(..line, col: 2), "e").state.col == 6
    expect step(Editor(..line, col: 8), "b").state.col == 4
    expect step(Editor(..line, col: 4), "b").state.col == 0
    // A count rides the existing machinery: 2w moves two words.
    expect step(Editor(..line, pending: Pending(op: OpNone, count: 2, object: NoObj)), "w").state.col == 8
}

// A motion's target position + wiseness, or `None` for a non-motion key. THE single
// motion definition — consumed both bare (`moveTo`, wiseness ignored) and as an
// operator's range endpoint (Phase 2.3+). One `motionTarget` powers `l` and `dl`.
motionTarget(state, key) = match {
    key == "h" => Some(Target(row: state.row, col: state.col - 1, wise: Charwise, inclusive: false))
    key == "l" => Some(Target(row: state.row, col: state.col + 1, wise: Charwise, inclusive: false))
    key == "0" => Some(Target(row: state.row, col: 0, wise: Charwise, inclusive: false))
    key == "$" => Some(Target(row: state.row, col: lineEndCol(state), wise: Charwise, inclusive: true))
    key == "^" => Some(Target(row: state.row, col: firstNonBlankCol(state), wise: Charwise, inclusive: false))
    key == "j" => Some(Target(row: clampRow(state, state.row + 1), col: state.col, wise: Linewise, inclusive: false))
    key == "k" => Some(Target(row: clampRow(state, state.row - 1), col: state.col, wise: Linewise, inclusive: false))
    key == "w" => Some(Target(row: state.row, col: wordForwardCol(lineAt(state, state.row), state.col), wise: Charwise, inclusive: false))
    key == "e" => Some(Target(row: state.row, col: wordEndCol(lineAt(state, state.row), state.col), wise: Charwise, inclusive: true))
    key == "b" => Some(Target(row: state.row, col: wordBackCol(lineAt(state, state.row), state.col), wise: Charwise, inclusive: false))
    _ => None
}

// `motionTarget` is the single definition the bare move and the operator both
// consume, so its target *and* its wiseness are what the rest of the motion
// machinery is built on. A non-motion key must yield `None` rather than a target
// that happens to sit under the cursor — the difference between "no motion" and
// "a motion that goes nowhere" is what tells `step` whether the key was handled.
test "motion target carries position and wiseness" {
    // `$` is a charwise target at the last char (col 4 of "hello").
    expect match motionTarget(initialState("hello"), "$") {
        Some(t) => t.col == 4 and t.wise == Charwise
        None => false
    }
    // `j` is a linewise target on the next row, carrying the column across.
    expect match motionTarget(Editor(..initialState("ab\ncd"), col: 1), "j") {
        Some(t) => t.row == 1 and t.col == 1 and t.wise == Linewise
        None => false
    }
    expect match motionTarget(initialState("hello"), "z") {
        Some(_) => false
        None => true
    }
}

// Move the cursor to `target`, clamping the column onto the target row's `[0, len]`.
// A vertical move re-clamps onto a shorter line; `h` past column 0 clamps back up.
moveTo(state, target) = {
    let lineLen = Str.length(lineAt(state, target.row))
    let col = target.col < 0 => 0 | (target.col > lineLen => lineLen | target.col)
    Editor(..state, row: target.row, col: col)
}

// Apply a motion key as a bare cursor move (ignoring wiseness); a no-op for a
// non-motion key, whose target defaults to the current cursor.
moveByKey(state, key) = moveTo(state, unwrapOr(motionTarget(state, key), Target(row: state.row, col: state.col, wise: Charwise, inclusive: false)))

// `k` / `j` — named wrappers over the shared motion machinery.
moveUp(state) = moveByKey(state, "k")
moveDown(state) = moveByKey(state, "j")

test "j moves down a line and k moves up" {
    expect moveDown(initialState("a\nb\nc")).row == 1
    expect moveUp(moveDown(initialState("a\nb\nc"))).row == 0
}

test "j at the last line and k at the top are no-ops" {
    expect moveUp(initialState("a\nb")).row == 0
    expect moveDown(moveDown(initialState("a\nb"))).row == 1
}

// A vertical move re-clamps onto the target line's length, so the column is not
// simply carried across.
test "moving onto a shorter line reclamps the column" {
    expect moveDown(Editor(..initialState("aaa\nb"), col: 2)).col == 1
    // A full-width target leaves the column alone.
    expect moveDown(Editor(..initialState("aaa\nccc"), col: 2)).col == 2
}

test "h and l move the cursor within the line" {
    let mid = Editor(..initialState("abc"), col: 1)
    expect step(mid, "l").state.col == 2
    expect step(mid, "h").state.col == 0
    // `h` at column 0 and `l` at the line's end are both no-ops (clamped).
    expect step(initialState("abc"), "h").state.col == 0
    expect step(Editor(..initialState("abc"), col: 3), "l").state.col == 3
    // They are handled keys, so they redraw — even the clamped no-op.
    expect step(mid, "h").effect == Redraw
}

test "zero and dollar jump to the line ends" {
    let mid = Editor(..initialState("hello"), col: 2)
    expect step(mid, "0").state.col == 0
    // `$` lands *on* the last character (len - 1), unlike `A` which appends at len.
    expect step(mid, "$").state.col == 4
    // On an empty line `$` clamps to col 0 — max(len - 1, 0), never -1.
    expect step(initialState(""), "$").state.col == 0
    // Pure motions redraw rather than reporting an edit: nothing in the buffer
    // changed, and a caller that flushes on `Edit` must not be told to.
    expect step(mid, "0").effect == Redraw
    expect step(mid, "$").effect == Redraw
}

test "caret jumps to the first non-blank column" {
    // "  hi" → `^` lands on col 2 (the 'h'), skipping the leading spaces.
    expect step(Editor(..initialState("  hi"), col: 3), "^").state.col == 2
    // A line with no content (all spaces) clamps `^` to col 0.
    expect step(Editor(..initialState("   "), col: 2), "^").state.col == 0
    // No leading blanks → `^` is column 0, same as `0`.
    expect step(Editor(..initialState("hi"), col: 1), "^").state.col == 0
    expect step(Editor(..initialState("  hi"), col: 3), "^").effect == Redraw
}

// `x` — delete the character under the cursor. No-op at end of line (nothing under
// the cursor). Cuts into the register and re-clamps the cursor.
deleteChar(state) = deleteCharRange(state, state.row, state.col, state.col + 1)

// `X` — delete the character *before* the cursor (the mirror of `x`). No-op at
// column 0, where nothing precedes the cursor.
deleteBefore(state) = deleteCharRange(state, state.row, state.col - 1, state.col)

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

// `D` — delete from the cursor to the end of the line, keeping `[0, col)`. Cuts into
// the register and re-clamps the cursor.
deleteToEol(state) = deleteCharRange(state, state.row, state.col, Str.length(lineAt(state, state.row)))

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
insertHead(state) = enterInsert(Editor(..state, col: firstNonBlankCol(state)))

// `O` — open a fresh empty line *above* the cursor and enter Insert there (the
// mirror of `o`). Inserting at `row` pushes the current line and its successors down.
openAbove(state) = Editor(
    ..state,
    lines: List.insert(state.lines, state.row, ""),
    col: 0,
    mode: Insert,
)

// Insert every line of `newLines` into `lines` starting at index `at`, in order
// (each insert shifts the tail, so successive indices `at + i` land contiguously).
insertLinesFrom(lines, at, newLines, i) =
    i >= count(newLines) => lines
    | insertLinesFrom(List.insert(lines, at + i, unwrapOr(List.at(newLines, i), "")), at, newLines, i + 1)

// Paste the register's text into the current line at column `at`, cursor landing on
// the last pasted character.
pasteCharwise(state, at) = {
    let line = lineAt(state, state.row)
    let text = state.clipboard.text
    let edited = Str.slice(line, 0, at) + text + Str.slice(line, at, Str.length(line))
    Editor(..state, lines: List.set(state.lines, state.row, edited), col: at + Str.length(text) - 1)
}

// Paste the register's lines into the buffer at row `at`, cursor at the first pasted
// line. A multi-line register (from a linewise yank/delete of several lines) splits
// back into that many lines.
pasteLinewise(state, at) = Editor(
    ..state,
    lines: insertLinesFrom(state.lines, at, Str.split(state.clipboard.text, "\n"), 0),
    row: at,
    col: 0,
)

// Paste dispatched by the register's wiseness: `charAt` is the column for a charwise
// paste, `lineIdx` the row for a linewise one.
doPaste(state, charAt, lineIdx) = match state.clipboard.wise {
    Charwise => pasteCharwise(state, charAt)
    Linewise => pasteLinewise(state, lineIdx)
}

// `p` — paste after: charwise after the cursor (col + 1, clamped), linewise on a new
// line below.
pasteAfter(state) = {
    let len = Str.length(lineAt(state, state.row))
    let at = state.col + 1 > len => len | state.col + 1
    doPaste(state, at, state.row + 1)
}

// `P` — paste before: charwise at the cursor, linewise on a new line above.
pasteBefore(state) = doPaste(state, state.col, state.row)

// `Esc` — return to Normal mode. The inverse of `enterInsert`; buffer/cursor kept.
enterNormal(state) = Editor(..state, mode: Normal)

// `i` then `Esc` must return to *exactly* the starting state — a round-trip that
// proves the mode switch alone changed, and nothing else came along with it.
test "i enters insert and esc returns to normal" {
    let start = initialState("ab")
    expect enterInsert(start).mode == Insert
    let round = enterNormal(enterInsert(start))
    expect round.mode == Normal
    expect round.lines == start.lines
    expect round.row == start.row
    expect round.col == start.col
}

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

test "insert a printable char splits the line and advances the column" {
    expect insertChar(at("ac", 1), "b").lines == ["abc"]
    expect insertChar(at("ac", 1), "b").col == 2
    // At end of line, and at the start — the two boundaries a slice gets wrong.
    expect insertChar(at("ab", 2), "c").lines == ["abc"]
    expect insertChar(initialState("bc"), "a").lines == ["abc"]
    expect insertChar(initialState("bc"), "a").col == 1
}

// The edit must land on the *cursor's* row, not row 0 — an easy thing to get
// right by accident with a single-line fixture.
test "insert edits only the cursor row and leaves other lines" {
    let edited = insertChar(Editor(..initialState("xx\nyy"), row: 1, col: 1), "Z")
    expect edited.lines == ["xx", "yZy"]
    expect edited.col == 2
}

test "backspace deletes the char before the cursor" {
    expect backspace(at("abc", 2)).lines == ["ac"]
    expect backspace(at("abc", 2)).col == 1
}

// The join has to target row - 1 and remove row, rather than hardcoded indices,
// which a two-line fixture cannot distinguish.
test "backspace at col 0 joins onto the previous line" {
    let joined = backspace(Editor(..initialState("a\nb\nc"), row: 2, col: 0))
    expect joined.lines == ["a", "bc"]
    expect joined.row == 1
    expect joined.col == 1
}

test "backspace at the top left is a no-op" {
    expect backspace(initialState("ab")).lines == ["ab"]
    expect backspace(initialState("ab")).row == 0
    expect backspace(initialState("ab")).col == 0
}

test "enter splits the line at the cursor into two" {
    let mid = splitLine(at("abc", 1))
    expect mid.lines == ["a", "bc"]
    expect mid.row == 1
    expect mid.col == 0
    // At the end: a fresh empty line below. At the start: content pushed down.
    expect splitLine(at("abc", 3)).lines == ["abc", ""]
    expect splitLine(initialState("abc")).lines == ["", "abc"]
}

test "enter splits only the cursor line and keeps the rest" {
    let split = splitLine(Editor(..initialState("x\nabc\ny"), row: 1, col: 1))
    expect split.lines == ["x", "a", "bc", "y"]
    expect split.row == 2
}

// `splitLine` leaves the cursor at col 0 of the new line, exactly where
// `backspace` joins back — so the two compose to the original. A round-trip that
// catches an off-by-one in *either* transition, which asserting them separately
// does not.
test "enter then backspace is the identity" {
    let start = at("abc", 1)
    let round = backspace(splitLine(start))
    expect round.lines == start.lines
    expect round.row == start.row
    expect round.col == start.col
}

// The buffer serialized for saving: the lines joined back with newlines — the
// inverse of `initialState`'s split.
serialize(state) = Str.join(state.lines, "\n")

test "serialize joins the line buffer with newlines" {
    expect serialize(initialState("a\nb\nc")) == "a\nb\nc"
    // A single empty line serializes to the empty string, so "" round-trips.
    expect serialize(initialState("")) == ""
}

// `:w` — a `Save` effect carrying the whole serialized buffer. The state is
// unchanged (saving doesn't edit).
save(state) = Step(state: state, effect: Save(serialize(state)))

test "save yields a save effect carrying the serialized buffer" {
    expect savedText(save(initialState("a\nb")).effect) == "a\nb"
    // Saving does not edit: the Step carries the state through unchanged.
    expect save(initialState("a\nb")).state.lines == ["a", "b"]
    expect save(initialState("a\nb")).state.col == 0
}

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

// Overwrite the whole accumulator record.
setPending(state, op, count, object) = Editor(..state, pending: Pending(op: op, count: count, object: object))

// Enter operator-pending: record the operator, keep count/object, await a motion.
enterOp(state, op) = setPending(state, op, state.pending.count, state.pending.object)

// Set the text-object prefix (`i`/`a`) while operator-pending.
setObject(state, object) = setPending(state, state.pending.op, state.pending.count, object)

// Leave operator-pending: reset the whole accumulator (op, count, object).
clearPending(state) = setPending(state, OpNone, 0, NoObj)

// The count prefix in effect: an unset (0) count means 1.
effectiveCount(state) = state.pending.count <= 0 => 1 | state.pending.count

// Consume the count prefix (after a command uses it), keeping op/object.
clearCount(state) = setPending(state, state.pending.op, 0, state.pending.object)

// Apply `f` to `state` `n` times (n >= 1).
repeat(f, state, n) = n <= 1 => f(state) | repeat(f, f(state), n - 1)

// The integer value of a single digit key.
digitVal(key) = match {
    key == "1" => 1
    key == "2" => 2
    key == "3" => 3
    key == "4" => 4
    key == "5" => 5
    key == "6" => 6
    key == "7" => 7
    key == "8" => 8
    key == "9" => 9
    _ => 0
}

// Whether `key` is a count digit here: "1".."9" always; "0" only once a count is
// already accumulating (a leading "0" is the line-start motion).
isCountDigit(state, key) = match {
    key == "0" => state.pending.count > 0
    _ => Str.length(key) == 1 and Str.contains("123456789", key)
}

// Fold a digit key into the count accumulator.
foldDigit(state, key) = setPending(state, state.pending.op, state.pending.count * 10 + digitVal(key), state.pending.object)

// The whole point of `isCountDigit` taking `state`: "0" is two different keys
// depending on whether a count is already accumulating. Getting this wrong makes
// `0` silently a no-op mid-count, or breaks line-start entirely.
test "digits accumulate a count and zero is a motion when the count is empty" {
    expect step(step(initialState("x"), "1").state, "2").state.pending.count == 12
    // A leading 0 is the line-start motion, not a count digit.
    let mid = Editor(..initialState("abc"), col: 2)
    expect step(mid, "0").state.pending.count == 0
    expect step(mid, "0").state.col == 0
    // But 0 *after* a digit folds in: 1 then 0 is 10.
    expect step(step(initialState("x"), "1").state, "0").state.pending.count == 10
}

test "a count repeats motions and clears afterward" {
    // 3j descends three lines; a count past the last line clamps rather than faulting.
    let down3 = step(Editor(..initialState("a\nb\nc\nd"), pending: Pending(op: OpNone, count: 3, object: NoObj)), "j").state
    expect down3.row == 3
    // A vertical move carries the column, so it is asserted alongside the row.
    expect down3.col == 0
    expect step(Editor(..initialState("a\nb"), pending: Pending(op: OpNone, count: 9, object: NoObj)), "j").state.row == 1
    expect step(Editor(..initialState("abcd"), pending: Pending(op: OpNone, count: 2, object: NoObj)), "l").state.col == 2
    // The count is consumed by the motion, so the next key starts clean.
    expect step(Editor(..initialState("a\nb\nc"), pending: Pending(op: OpNone, count: 2, object: NoObj)), "j").state.pending.count == 0
}

// Reset the count in a completed `Step`'s state (top-level commands consume it).
clearCountStep(step) = Step(state: clearCount(step.state), effect: step.effect)

// Delete a charwise (intra-line) column range `[lo, hi)` on `row`, clamped to the
// line, cutting the removed text into the unnamed register (charwise) and returning
// the cursor to the range start. An empty range leaves the register untouched.
deleteCharRange(state, row, lo, hi) = {
    let line = lineAt(state, row)
    let len = Str.length(line)
    let a = lo < 0 => 0 | (lo > len => len | lo)
    let b = hi < 0 => 0 | (hi > len => len | hi)
    let cut = Str.slice(line, a, b)
    let based = cut == "" => state | Editor(..state, clipboard: Register(text: cut, wise: Charwise))
    let moved = Editor(..based, lines: List.set(based.lines, row, Str.slice(line, 0, a) + Str.slice(line, b, len)), col: a)
    Editor(..moved, col: clampedCol(moved, row))
}

// The half-open charwise column range `[lo, hi)` between the cursor and a motion
// `target` on the same line. Backward → `[target, cursor)`; forward → `[cursor,
// target)`, extended by one for an inclusive motion (`$`).
charLo(state, target) = target.col < state.col => target.col | state.col
charHi(state, target) = target.col < state.col => state.col | (target.inclusive => target.col + 1 | target.col)

// Delete that charwise range on the cursor's line.
deleteCharwise(state, target) = deleteCharRange(state, state.row, charLo(state, target), charHi(state, target))

// The text of the charwise range (clamped to the line).
charRangeText(state, target) = {
    let line = lineAt(state, state.row)
    let len = Str.length(line)
    let a = charLo(state, target)
    let b = charHi(state, target)
    let a2 = a < 0 => 0 | (a > len => len | a)
    let b2 = b < 0 => 0 | (b > len => len | b)
    Str.slice(line, a2, b2)
}

// The text of whole lines `[lo, hi]` joined by newlines (recursive).
joinLinesRange(state, lo, hi) = match {
    lo > hi => ""
    lo == hi => lineAt(state, lo)
    _ => lineAt(state, lo) + "\n" + joinLinesRange(state, lo + 1, hi)
}

// The telemetry span name for an operator.
opSpanName(op) = match {
    op == OpDelete => "stim.delete"
    op == OpChange => "stim.change"
    _ => "stim.yank"
}

// Delete the range from the cursor to a motion `target`, dispatched by wiseness.
// Charwise = an intra-line span; linewise = whole lines `[min, max]` of the two rows.
// A linewise motion that didn't move (`dj` on the last line) selects a single
// unmoved line, which we treat as a no-op — `dd` is the way to delete one line.
deleteRange(state, target) = match target.wise {
    Charwise => deleteCharwise(state, target)
    Linewise =>
        target.row == state.row => state
        | deleteLines(state, (target.row < state.row => target.row | state.row), (target.row < state.row => state.row | target.row))
}

// The lo/hi whole-line bounds of a linewise motion (min/max of the two rows).
lineLo(state, target) = target.row < state.row => target.row | state.row
lineHi(state, target) = target.row < state.row => state.row | target.row

// Yank the range from the cursor to a motion `target` into the unnamed register,
// without editing the buffer. Charwise = the intra-line span; linewise = whole lines.
// An unmoved linewise motion selects nothing (like delete).
yankRange(state, target) = match target.wise {
    Charwise => Editor(..state, clipboard: Register(text: charRangeText(state, target), wise: Charwise))
    Linewise =>
        target.row == state.row => state
        | Editor(..state, clipboard: Register(text: joinLinesRange(state, lineLo(state, target), lineHi(state, target)), wise: Linewise))
}

// Replace whole lines `[lo, hi]` with a single empty line and enter Insert at its
// start — the linewise change (`cc`/`cj`). Unlike delete, it KEEPS a line.
changeLines(state, lo, hi) = {
    let removed = removeLinesFrom(state.lines, lo, hi - lo + 1)
    Editor(..state, lines: List.insert(removed, lo, ""), row: lo, col: 0, mode: Insert)
}

// Change the range from the cursor to a motion `target`: charwise = delete the span
// then enter Insert; linewise = blank the lines then enter Insert. An unmoved
// linewise motion changes nothing.
changeRange(state, target) = match target.wise {
    Charwise => enterInsert(deleteCharwise(state, target))
    Linewise =>
        target.row == state.row => state
        | changeLines(state, lineLo(state, target), lineHi(state, target))
}

// Apply the pending operator over the range from the cursor to a motion `target`.
applyMotionOp(state, target) = match state.pending.op {
    OpDelete => deleteRange(state, target)
    OpChange => changeRange(state, target)
    OpYank => yankRange(state, target)
    _ => state
}

// An operator-pending `Editor` at `col` of "abcd", awaiting a motion.
pendingDelete(text, col) = Editor(..initialState(text), col: col, pending: Pending(op: OpDelete, count: 0, object: NoObj))

// Whether an effect reports a buffer edit. `Edit` carries the serialized buffer,
// so it is matched rather than compared — the tag is what these tests assert.
isEdit(effect) = match effect { Edit(_) => true  _ => false }

// `d` alone must not act. The operator is a *mode within Normal*: it records what
// to do and waits for the motion that says where, so the key that sets it changes
// nothing but `pending`.
test "operators enter operator-pending within normal mode" {
    expect initialState("abc").pending.op == OpNone
    expect step(initialState("abc"), "d").state.mode == Normal
    expect step(initialState("abc"), "d").effect == Redraw
    expect step(initialState("abc"), "d").state.pending.op == OpDelete
    expect step(initialState("abc"), "c").state.pending.op == OpChange
    expect step(initialState("abc"), "y").state.pending.op == OpYank
}

// The charwise operators, each against the motion that defines its span. `dl` and
// `dh` are the interesting pair: same one-character span, opposite directions, and
// only `dh` moves the cursor.
test "d plus a charwise motion deletes the intra-line range" {
    // d$ deletes through end-of-line, inclusive — the same result as D.
    expect step(pendingDelete("abcd", 1), "$").state.lines == ["a"]
    // dl deletes the char under the cursor (exclusive, forward), like x.
    expect step(pendingDelete("abcd", 1), "l").state.lines == ["acd"]
    // dh deletes the char before it (backward), like X — and the cursor follows.
    expect step(pendingDelete("abcd", 1), "h").state.lines == ["bcd"]
    expect step(pendingDelete("abcd", 1), "h").state.col == 0
    // d0 deletes from line-start up to the cursor, exclusive of the char under it.
    expect step(pendingDelete("abcd", 2), "0").state.lines == ["cd"]
    // The operator clears after applying, and a real delete is an observable Edit.
    expect step(pendingDelete("abcd", 1), "l").state.pending.op == OpNone
    expect isEdit(step(pendingDelete("abcd", 1), "l").effect)
    // dl at end of line deletes nothing — a Redraw, and pending still clears, or the
    // next keystroke would be swallowed by a stale operator.
    expect step(pendingDelete("ab", 2), "l").effect == Redraw
    expect step(pendingDelete("ab", 2), "l").state.pending.op == OpNone
}

// The doubled operator is the linewise self: `dd` removes the current line.
test "dd deletes the whole current line" {
    let mid = Editor(..initialState("a\nb\nc"), row: 1, pending: Pending(op: OpDelete, count: 0, object: NoObj))
    expect step(mid, "d").state.lines == ["a", "c"]
    // The cursor stays on the row — now the line that was below.
    expect step(mid, "d").state.row == 1
    // dd on the last line clamps the cursor up to the new last line.
    let last = Editor(..initialState("a\nb\nc"), row: 2, pending: Pending(op: OpDelete, count: 0, object: NoObj))
    expect step(last, "d").state.lines == ["a", "b"]
    expect step(last, "d").state.row == 1
    // dd on the only line leaves one empty line: the buffer never reaches zero lines.
    expect step(pendingDelete("abc", 0), "d").state.lines == [""]
    expect isEdit(step(pendingDelete("a\nb", 0), "d").effect)
    expect step(pendingDelete("a\nb", 0), "d").state.pending.op == OpNone
}

test "d plus a linewise motion deletes a range of lines" {
    // dj takes the current line and the one below — the range [row, row + 1].
    let top = Editor(..initialState("a\nb\nc"), row: 0, pending: Pending(op: OpDelete, count: 0, object: NoObj))
    expect step(top, "j").state.lines == ["c"]
    // dk takes the current line and the one above.
    let bottom = Editor(..initialState("a\nb\nc"), row: 2, pending: Pending(op: OpDelete, count: 0, object: NoObj))
    expect step(bottom, "k").state.lines == ["a"]
    expect isEdit(step(top, "j").effect)
    // dj on the last line: `j` cannot move, so the range is one unmoved line, which
    // is a no-op rather than a line delete — `dd` is how you delete one line.
    let lastRow = Editor(..initialState("a\nb"), row: 1, pending: Pending(op: OpDelete, count: 0, object: NoObj))
    expect step(lastRow, "j").effect == Redraw
    expect step(lastRow, "j").state.pending.op == OpNone
}

// `c` is `d` plus Insert, with one deliberate asymmetry: `cc` keeps the line.
test "c changes a range and enters insert" {
    let cDollar = Editor(..initialState("abcd"), col: 1, pending: Pending(op: OpChange, count: 0, object: NoObj))
    expect step(cDollar, "$").state.lines == ["a"]
    expect step(cDollar, "$").state.mode == Insert
    expect isEdit(step(cDollar, "$").effect)
    // cc blanks the line and enters Insert — it KEEPS the line, unlike dd.
    let cc = Editor(..initialState("a\nb\nc"), row: 1, pending: Pending(op: OpChange, count: 0, object: NoObj))
    expect step(cc, "c").state.lines == ["a", "", "c"]
    expect step(cc, "c").state.row == 1
    expect step(cc, "c").state.mode == Insert
}

test "y yanks a range into the clipboard without deleting" {
    let yy = step(Editor(..initialState("a\nb\nc"), row: 1, pending: Pending(op: OpYank, count: 0, object: NoObj)), "y").state
    // The buffer is untouched — that is the whole difference from `d`.
    expect yy.lines == ["a", "b", "c"]
    expect yy.clipboard.text == "b"
    expect yy.clipboard.wise == Linewise
    let yDollar = step(Editor(..initialState("abcd"), col: 1, pending: Pending(op: OpYank, count: 0, object: NoObj)), "$").state
    expect yDollar.clipboard.text == "bcd"
    expect yDollar.clipboard.wise == Charwise
}

test "capital Y yanks the current line" {
    let yanked = step(Editor(..initialState("a\nb"), row: 0), "Y").state
    expect yanked.clipboard.text == "a"
    expect yanked.clipboard.wise == Linewise
    expect yanked.lines == ["a", "b"]
}

// The register's wiseness decides *where* a paste lands, not just what it holds:
// charwise goes inline, linewise opens a line. Same key, two behaviours.
test "p and capital P paste charwise from the clipboard" {
    let clip = Editor(..initialState("ac"), col: 0, clipboard: Register(text: "X", wise: Charwise))
    // p inserts after the cursor; the cursor lands on the pasted char.
    expect step(clip, "p").state.lines == ["aXc"]
    expect step(clip, "p").state.col == 1
    // P inserts before it.
    expect step(clip, "P").state.lines == ["Xac"]
    expect step(clip, "P").state.col == 0
    expect isEdit(step(clip, "p").effect)
    // An empty (never-yanked) register pastes nothing.
    expect step(initialState("ac"), "p").effect == Redraw
}

test "p and capital P paste linewise opening a line" {
    let clip = Editor(..initialState("a\nb"), row: 0, clipboard: Register(text: "X", wise: Linewise))
    expect step(clip, "p").state.lines == ["a", "X", "b"]
    expect step(clip, "p").state.row == 1
    expect step(clip, "P").state.lines == ["X", "a", "b"]
    expect step(clip, "P").state.row == 0
    // A multi-line register pastes all of its lines.
    let multi = Editor(..initialState("a"), row: 0, clipboard: Register(text: "X\nY", wise: Linewise))
    expect step(multi, "p").state.lines == ["a", "X", "Y"]
}

// Cut-and-paste only works if a delete populates the register, and only stays
// usable if a *no-op* delete leaves it alone.
test "a delete populates the clipboard so paste can move it" {
    let afterDd = step(pendingDelete("a\nb\nc", 0), "d").state
    expect afterDd.clipboard.text == "a"
    expect afterDd.clipboard.wise == Linewise
    let afterX = step(initialState("abc"), "x").state
    expect afterX.clipboard.text == "a"
    expect afterX.clipboard.wise == Charwise
    // x at end of line deletes nothing and must not clobber the register.
    let noop = Editor(..initialState("ab"), col: 2, clipboard: Register(text: "keep", wise: Charwise))
    expect step(noop, "x").state.clipboard.text == "keep"
}

// A pending operator must be abandonable, and abandoning it must leave the editor
// *clean* — the key that cancels is swallowed rather than run as a Normal command,
// or `dz` would delete nothing and then do whatever `z` does.
test "esc and unknown keys cancel operator-pending" {
    let pend = pendingDelete("abc", 1)
    expect step(pend, "Esc").state.mode == Normal
    expect step(pend, "Esc").effect == Redraw
    expect step(pend, "Esc").state.pending.op == OpNone
    expect step(pend, "Esc").state.lines == ["abc"]
    // An unknown key cancels the same way, and is not then executed.
    expect step(pend, "z").effect == Redraw
    expect step(pend, "z").state.pending.op == OpNone
    expect step(pend, "z").state.lines == ["abc"]
    // After cancelling, the next key acts normally.
    expect step(step(pend, "z").state, "x").state.lines == ["ac"]
}

// A count scales whatever follows it, whether that is a bare command or an
// operator's motion — one accumulator, three consumers.
test "a count repeats x and scales operators" {
    expect step(Editor(..initialState("abcdef"), pending: Pending(op: OpNone, count: 3, object: NoObj)), "x").state.lines == ["def"]
    // 2dd takes two whole lines (count on the doubled operator).
    expect step(Editor(..initialState("a\nb\nc\nd"), pending: Pending(op: OpDelete, count: 2, object: NoObj)), "d").state.lines == ["c", "d"]
    // d2j takes the current line plus two below (count on the linewise motion).
    expect step(Editor(..initialState("a\nb\nc\nd"), pending: Pending(op: OpDelete, count: 2, object: NoObj)), "j").state.lines == ["d"]
}

// Remove `n` whole lines starting at `lo` from `lines` (tail-recursive).
removeLinesFrom(lines, lo, n) =
    n <= 0 => lines | removeLinesFrom(List.removeAt(lines, lo), lo, n - 1)

// Delete whole lines `[lo, hi]` (inclusive). The buffer never goes to zero lines —
// deleting everything leaves one empty line. The cursor drops to column 0 of the
// line that now sits at `lo`, clamped to the last line.
deleteLines(state, lo, hi) = {
    let cut = joinLinesRange(state, lo, hi)
    let remaining = removeLinesFrom(state.lines, lo, hi - lo + 1)
    let lines2 = count(remaining) <= 0 => [""] | remaining
    let last = count(lines2) - 1
    let row = lo > last => last | lo
    Editor(..state, lines: lines2, row: row, col: 0, clipboard: Register(text: cut, wise: Linewise))
}

// Whether `key` re-presses the pending operator — the doubled-operator case (`dd`).
isSameOp(op, key) = match {
    op == OpDelete => key == "d"
    op == OpChange => key == "c"
    op == OpYank => key == "y"
    _ => false
}

// Yank whole lines `[lo, hi]` into the unnamed register, linewise (`yy`/`Y`, and
// counted `3yy`/`3Y`).
yankLinesRange(state, lo, hi) = Editor(..state, clipboard: Register(text: joinLinesRange(state, lo, hi), wise: Linewise))

// The last row a linewise count reaches from the cursor (`Ndd` spans N lines),
// clamped to the buffer.
countLastRow(state) = clampRow(state, state.row + effectiveCount(state) - 1)

// The doubled operator (`dd`/`cc`/`yy`, and counted `2dd`): apply the pending
// operator linewise to `count` lines. dd removes them; cc blanks-and-inserts (keeps
// one line); yy copies them.
applyLinewiseSelf(state) = match state.pending.op {
    OpDelete => editIfChanged(clearPending(state), clearPending(deleteLines(state, state.row, countLastRow(state))), "stim.delete")
    OpChange => edit(clearPending(changeLines(state, state.row, countLastRow(state))), "stim.change")
    OpYank => editIfChanged(clearPending(state), clearPending(yankLinesRange(state, state.row, countLastRow(state))), "stim.yank")
    _ => redraw(clearPending(state))
}

// --- Text objects (Phase 4b). A text object yields a cursor-independent `Range`
// that an operator consumes. v1: the word object `iw`/`aw`, single-row charwise. ---

// Whether the character at column `i` is a space.
isSpaceAt(line, i) = Str.slice(line, i, i + 1) == " "

// Backward: the start of the maximal same-class (space vs non-space) run containing
// `i`, given the class `isSp` of the run.
sameClassBack(line, i, isSp) = match {
    i <= 0 => 0
    isSpaceAt(line, i - 1) == isSp => sameClassBack(line, i - 1, isSp)
    _ => i
}

// Forward: the first index at/after `i` whose class differs from `isSp` (or `len`) —
// the exclusive end of a same-class run.
sameClassFwd(line, i, len, isSp) = match {
    i >= len => i
    isSpaceAt(line, i) == isSp => sameClassFwd(line, i + 1, len, isSp)
    _ => i
}

// `iw` — the maximal same-class run (a word, or a whitespace gap) containing the
// cursor. Degenerate (empty) if the cursor is past the last character.
innerWordRange(state) = {
    let line = lineAt(state, state.row)
    let len = Str.length(line)
    let c = state.col
    c >= len => Range(startRow: state.row, startCol: c, endRow: state.row, endCol: c, wise: Charwise)
    | {
        let isSp = isSpaceAt(line, c)
        Range(startRow: state.row, startCol: sameClassBack(line, c, isSp), endRow: state.row, endCol: sameClassFwd(line, c, len, isSp), wise: Charwise)
    }
}

// `aw` — the inner word plus its trailing whitespace; if there is none (word at end
// of line), the leading whitespace instead — vim's rule.
aroundWordRange(state) = {
    let inner = innerWordRange(state)
    let line = lineAt(state, state.row)
    let len = Str.length(line)
    let afterSpaces = sameClassFwd(line, inner.endCol, len, true)
    afterSpaces > inner.endCol => Range(startRow: inner.startRow, startCol: inner.startCol, endRow: inner.endRow, endCol: afterSpaces, wise: Charwise)
    | Range(startRow: inner.startRow, startCol: sameClassBack(line, inner.startCol, true), endRow: inner.endRow, endCol: inner.endCol, wise: Charwise)
}

// The word object for the pending `i`/`a` kind.
wordObject(state) = match state.pending.object {
    Around => aroundWordRange(state)
    _ => innerWordRange(state)
}

// An `Editor` mid-way through an operator + text-object sequence, awaiting the
// object key. The operator tests build this by hand a dozen times over.
pendingObject(text, col, op, object) = Editor(..initialState(text), col: col, pending: Pending(op: op, count: 0, object: object))

// A word motion under an operator selects the motion's span, which is what makes
// `dw` and `w` share one definition rather than two that drift.
test "operators consume word motions" {
    expect step(pendingDelete("foo bar", 0), "w").state.lines == ["bar"]
    // dw on the last word deletes to end of line — the motion clamps within it.
    expect step(pendingDelete("foo bar", 4), "w").state.lines == ["foo "]
    // cw changes over the same range and enters Insert.
    expect step(pendingObject("foo bar", 0, OpChange, NoObj), "w").state.mode == Insert
}

// `i`/`a` after an operator are not motions — they extend the pending state a
// second time, so the operator survives and the editor is still waiting.
test "i or a after an operator enters text-object pending" {
    let d = pendingDelete("foo bar", 0)
    expect step(d, "i").state.pending.object == Inner
    expect step(d, "a").state.pending.object == Around
    expect step(d, "i").state.pending.op == OpDelete
    expect step(d, "i").effect == Redraw
}

// The hallmark of a text object: the range does not depend on where in the word
// the cursor sits. "foo bar baz" has "bar" at cols 4..6, so mid-word and
// start-of-word must give the same result.
test "diw deletes the inner word from anywhere in it" {
    let mid = pendingObject("foo bar baz", 5, OpDelete, Inner)
    let start = pendingObject("foo bar baz", 4, OpDelete, Inner)
    expect step(mid, "w").state.lines == ["foo  baz"]
    expect step(start, "w").state.lines == ["foo  baz"]
    expect isEdit(step(mid, "w").effect)
    // The whole accumulator clears, object as well as operator.
    expect step(mid, "w").state.pending.object == NoObj
    expect step(mid, "w").state.pending.op == OpNone
}

test "ciw changes and yiw yanks the inner word" {
    let ci = pendingObject("foo bar", 5, OpChange, Inner)
    expect step(ci, "w").state.lines == ["foo "]
    expect step(ci, "w").state.mode == Insert
    // yiw copies without touching the buffer.
    let yi = pendingObject("foo bar", 5, OpYank, Inner)
    expect step(yi, "w").state.clipboard.text == "bar"
    expect step(yi, "w").state.lines == ["foo bar"]
}

// `aw` takes the trailing whitespace, or the leading whitespace when there is no
// trailing — vim's rule, and the reason `aw` is not just `iw` plus one space.
test "daw deletes a word with its surrounding whitespace" {
    expect step(pendingObject("foo bar baz", 5, OpDelete, Around), "w").state.lines == ["foo baz"]
    expect step(pendingObject("foo bar", 5, OpDelete, Around), "w").state.lines == ["foo"]
}

// The dispatch wiring end to end: three separate keystrokes, one edit.
test "the full d i w key sequence deletes the inner word" {
    let after = step(step(step(Editor(..initialState("foo bar"), col: 5), "d").state, "i").state, "w").state
    expect after.lines == ["foo "]
    expect after.col == 4
}

// Forward: the first index at/after `i` holding `ch` (or `len` if none).
nextCharAt(line, i, len, ch) = match {
    i >= len => len
    Str.slice(line, i, i + 1) == ch => i
    _ => nextCharAt(line, i + 1, len, ch)
}

// The opening-delimiter index of the `ch`-pair enclosing or after the cursor (pairs
// counted left to right: 1st–2nd, 3rd–4th, …), or -1 if there is no complete pair.
findDelimOpen(line, from, len, c, ch) = {
    let o = nextCharAt(line, from, len, ch)
    o >= len => 0 - 1 | {
        let close = nextCharAt(line, o + 1, len, ch)
        match {
            close >= len => 0 - 1
            c <= close => o
            _ => findDelimOpen(line, close + 1, len, c, ch)
        }
    }
}

// The `Range` for a delimiter pair `[o, close]`: `Inner` = between them, `Around` =
// including them.
delimRange(state, o, close) = match state.pending.object {
    Around => Range(startRow: state.row, startCol: o, endRow: state.row, endCol: close + 1, wise: Charwise)
    _ => Range(startRow: state.row, startCol: o + 1, endRow: state.row, endCol: close, wise: Charwise)
}

// The `Range` for the `ch`-delimited pair around the cursor, or `None` if there is no
// pair on the line.
delimObjectRange(state, ch) = {
    let line = lineAt(state, state.row)
    let len = Str.length(line)
    let o = findDelimOpen(line, 0, len, state.col, ch)
    o < 0 => None | Some(delimRange(state, o, nextCharAt(line, o + 1, len, ch)))
}

// A quote object is found by scanning the *line* for a pair, so unlike a word
// object it works from outside the quotes as well as inside — the cursor picks
// which pair, not whether there is one.
test "quote objects target the text in quotes" {
    // foo "bar" baz — quotes at cols 4 and 8, "bar" at 5..7.
    expect step(pendingObject("foo \"bar\" baz", 6, OpDelete, Inner), "\"").state.lines == ["foo \"\" baz"]
    // The same, with the cursor *before* the string on that line.
    expect step(pendingObject("foo \"bar\" baz", 1, OpDelete, Inner), "\"").state.lines == ["foo \"\" baz"]
    // da" takes the quotes too, leaving the surrounding spaces.
    expect step(pendingObject("foo \"bar\" baz", 6, OpDelete, Around), "\"").state.lines == ["foo  baz"]
}

test "ci quote changes and yi quote yanks the quoted text" {
    let ci = pendingObject("say \"hi\" now", 5, OpChange, Inner)
    expect step(ci, "\"").state.lines == ["say \"\" now"]
    expect step(ci, "\"").state.mode == Insert
    let yi = pendingObject("say \"hi\" now", 5, OpYank, Inner)
    expect step(yi, "\"").state.clipboard.text == "hi"
}

// No pair on the line means the object resolves to nothing, which must cancel
// rather than delete a degenerate range.
test "a quote object with no quotes on the line cancels" {
    let di = pendingObject("no quotes here", 0, OpDelete, Inner)
    expect step(di, "\"").effect == Redraw
    expect step(di, "\"").state.lines == ["no quotes here"]
}

test "single quote object targets apostrophe-delimited text" {
    expect step(pendingObject("a 'x' b", 3, OpDelete, Inner), "'").state.lines == ["a '' b"]
}

// The `Range` for a text-object char, or `None` if the key isn't an object. v1: word
// (`w`) and quotes (`"` / `'`); brackets are deferred.
textObjectRange(state, key) = match {
    key == "w" => Some(wordObject(state))
    key == "\"" => delimObjectRange(state, "\"")
    key == "'" => delimObjectRange(state, "'")
    _ => None
}

// The text of a charwise range `[lo, hi)` on `row`, clamped to the line.
rangeText(state, row, lo, hi) = {
    let line = lineAt(state, row)
    let len = Str.length(line)
    let a = lo < 0 => 0 | (lo > len => len | lo)
    let b = hi < 0 => 0 | (hi > len => len | hi)
    Str.slice(line, a, b)
}

// Apply the pending operator over an explicit `Range` (a text object), then clear the
// accumulator. v1 ranges are charwise single-row, so this reuses the charwise
// primitives. Measured against a pending-cleared baseline (like the motion path).
applyOpRange(state, range) = match state.pending.op {
    OpDelete => editIfChanged(clearPending(state), clearPending(deleteCharRange(state, range.startRow, range.startCol, range.endCol)), "stim.delete")
    OpChange => edit(clearPending(enterInsert(deleteCharRange(state, range.startRow, range.startCol, range.endCol))), "stim.change")
    OpYank => editIfChanged(clearPending(state), clearPending(Editor(..state, clipboard: Register(text: rangeText(state, range.startRow, range.startCol, range.endCol), wise: Charwise))), "stim.yank")
    _ => redraw(clearPending(state))
}

// Normal mode dispatch: object-pending (an `i`/`a` object awaiting its char) first,
// then a count digit, then the top-level table or the operator branch. `step` still
// dispatches on `mode` — this all lives inside Normal.
stepNormal(state, key) = match {
    state.pending.object != NoObj => stepObjectPending(state, key)
    isCountDigit(state, key) => redraw(foldDigit(state, key))
    state.pending.op == OpNone => stepTopLevel(state, key)
    _ => stepOperatorPending(state, key)
}

// Object-pending: the key is the object char (`w`, later `"`/`(`). Resolve the range
// and apply the pending operator; Esc or a non-object key cancels.
stepObjectPending(state, key) = match {
    key == "Esc" => redraw(clearPending(state))
    _ => match textObjectRange(state, key) {
        Some(range) => applyOpRange(state, range)
        None => redraw(clearPending(state))
    }
}

// The target `count` repetitions of a motion reach (`d3w`, `d2j`): the cursor after
// `n` bare moves, carrying the motion's wiseness/inclusive.
countedTarget(state, key, n, base) = {
    let moved = repeat(s -> moveByKey(s, key), state, n)
    Target(row: moved.row, col: moved.col, wise: base.wise, inclusive: base.inclusive)
}

// Apply the pending operator over a motion, scaled by the count, and clear the
// accumulator. Measured against a pending-cleared baseline so a no-op range spans
// nothing even though clearing the accumulator changed the state.
applyOpOverMotion(state, key, base) = {
    let target = effectiveCount(state) <= 1 => base | countedTarget(state, key, effectiveCount(state), base)
    editIfChanged(clearPending(state), clearPending(applyMotionOp(state, target)), opSpanName(state.pending.op))
}

// Operator-pending: a motion applies the pending operator over `[cursor, target]`
// (count-scaled) and clears the accumulator; the same operator again is the linewise
// self; Esc and unknown keys cancel.
stepOperatorPending(state, key) = match {
    key == "Esc" => redraw(clearPending(state))
    isSameOp(state.pending.op, key) => applyLinewiseSelf(state)
    _ => match motionTarget(state, key) {
        Some(target) => applyOpOverMotion(state, key, target)
        None => stepObjectOrCancel(state, key)
    }
}

// A non-motion, non-operator key while operator-pending: `i`/`a` begin a text object
// (await its char); anything else cancels the operator.
stepObjectOrCancel(state, key) = match {
    key == "i" => redraw(setObject(state, Inner))
    key == "a" => redraw(setObject(state, Around))
    _ => redraw(clearPending(state))
}

// The top-level Normal dispatch. Operators `d`/`c`/`y` enter operator-pending
// *carrying* the count (so `2dd` works); every other command consumes the count via
// `clearCountStep`. The body applies the count where it multiplies (motions, `x`,
// paste).
stepTopLevel(state, key) = match {
    key == "d" => redraw(enterOp(state, OpDelete))
    key == "c" => redraw(enterOp(state, OpChange))
    key == "y" => redraw(enterOp(state, OpYank))
    _ => clearCountStep(stepTopLevelBody(state, key))
}

// Repeat a bare motion `key` by the count.
countedMotion(state, key) = repeat(s -> moveByKey(s, key), state, effectiveCount(state))

stepTopLevelBody(state, key) = match {
    key == "j" => redraw(countedMotion(state, key))
    key == "k" => redraw(countedMotion(state, key))
    key == "h" => redraw(countedMotion(state, key))
    key == "l" => redraw(countedMotion(state, key))
    key == "w" => redraw(countedMotion(state, key))
    key == "e" => redraw(countedMotion(state, key))
    key == "b" => redraw(countedMotion(state, key))
    key == "0" => redraw(moveByKey(state, key))
    key == "$" => redraw(moveByKey(state, key))
    key == "^" => redraw(moveByKey(state, key))
    key == "x" => editIfChanged(state, repeat(deleteChar, state, effectiveCount(state)), "stim.delete-char")
    key == "X" => editIfChanged(state, deleteBefore(state), "stim.delete-char")
    key == "~" => editIfChanged(state, toggleCase(state), "stim.toggle-case")
    key == "J" => editIfChanged(state, joinLines(state), "stim.join")
    key == "D" => editIfChanged(state, deleteToEol(state), "stim.delete-to-eol")
    key == "C" => edit(changeToEol(state), "stim.change-to-eol")
    key == "s" => edit(substitute(state), "stim.substitute")
    key == "Y" => editIfChanged(state, yankLinesRange(state, state.row, countLastRow(state)), "stim.yank")
    key == "p" => editIfChanged(state, repeat(pasteAfter, state, effectiveCount(state)), "stim.paste")
    key == "P" => editIfChanged(state, repeat(pasteBefore, state, effectiveCount(state)), "stim.paste")
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

// An `Editor` at `col` of `text`, in Normal mode — the setup nearly every
// single-key command test needs.
at(text, col) = Editor(..initialState(text), col: col)

// The text a `Save` effect carries, or "?" for any other effect.
savedText(effect) = match effect { Save(t) => t  _ => "?" }

// `step` is the only entry point, so mode dispatch is the first thing that can be
// wrong: an unbound key must be a `Noop` that changes nothing, not a silent
// fall-through into another mode's table.
test "step dispatches normal-mode keys" {
    let start = initialState("a\nb")
    expect step(start, "j").effect == Redraw
    expect step(start, "j").state.row == 1
    expect step(start, "i").state.mode == Insert
    expect step(start, ":").state.mode == Command
    expect step(start, "z").effect == Noop
    expect step(start, "z").state.lines == start.lines
    expect step(start, "z").state.col == start.col
}

test "x deletes the character under the cursor" {
    expect step(at("abc", 1), "x").state.lines == ["ac"]
    expect step(at("abc", 1), "x").state.col == 1
    // At end of line there is nothing under the cursor, so nothing happens.
    expect step(at("abc", 3), "x").state.lines == ["abc"]
    // Deleting the last char re-clamps the cursor onto the shortened line.
    expect step(at("ab", 1), "x").state.lines == ["a"]
    expect step(at("ab", 1), "x").state.col == 1
}

// `X` is `x`'s mirror, and the pair is where the Edit-vs-Redraw distinction earns
// its keep: telemetry should span real edits only, so a no-op must not report one.
test "capital X deletes the char before the cursor as an observable edit" {
    expect step(at("abc", 2), "X").state.lines == ["ac"]
    expect step(at("abc", 2), "X").state.col == 1
    // At col 0 there is nothing before the cursor.
    expect step(at("abc", 0), "X").state.lines == ["abc"]
    expect isEdit(step(at("abc", 2), "X").effect)
    expect step(at("abc", 0), "X").effect == Redraw
}

test "o opens a line below and enters insert" {
    expect step(initialState("ab"), "o").state.lines == ["ab", ""]
    expect step(initialState("ab"), "o").state.row == 1
    expect step(initialState("ab"), "o").state.mode == Insert
}

test "capital O opens a line above and enters insert" {
    let second = Editor(..initialState("x\ny"), row: 1)
    expect step(second, "O").state.lines == ["x", "", "y"]
    expect step(second, "O").state.row == 1
    expect step(initialState("ab"), "O").state.mode == Insert
    expect isEdit(step(initialState("ab"), "O").effect)
}

// The three cursor-positioning entries into Insert. None of them edits, which is
// what separates them from `o`/`O`.
test "a capital A and capital I position the cursor and enter insert" {
    expect step(initialState("ab"), "a").state.col == 1
    expect step(initialState("ab"), "a").state.mode == Insert
    // `a` at end of line clamps at len — it cannot advance past the buffer.
    expect step(at("ab", 2), "a").state.col == 2
    // `A` jumps to end-of-line; `I` to the first non-blank column.
    expect step(initialState("ab"), "A").state.col == 2
    expect step(at("  ab", 3), "I").state.col == 2
    expect step(initialState("ab"), "a").effect == Redraw
    expect step(initialState("ab"), "A").effect == Redraw
    expect step(at("  ab", 3), "I").effect == Redraw
}

test "tilde toggles the case under the cursor and advances" {
    expect step(initialState("aBc"), "~").state.lines == ["ABc"]
    expect step(initialState("aBc"), "~").state.col == 1
    // On an uppercase char it flips the other way.
    expect step(at("aBc", 1), "~").state.lines == ["abc"]
    expect step(at("aBc", 1), "~").state.col == 2
    // At end of line there is nothing to toggle.
    expect step(at("aBc", 3), "~").effect == Redraw
    expect isEdit(step(initialState("aBc"), "~").effect)
}

test "capital J joins the next line onto the current one" {
    expect step(initialState("ab\ncd"), "J").state.lines == ["ab cd"]
    // The cursor lands at the seam.
    expect step(initialState("ab\ncd"), "J").state.col == 2
    // The next line's leading blanks are collapsed to the single seam space.
    expect step(initialState("ab\n   cd"), "J").state.lines == ["ab cd"]
    // On the last line there is nothing to join.
    expect step(Editor(..initialState("ab\ncd"), row: 1), "J").effect == Redraw
    expect isEdit(step(initialState("ab\ncd"), "J").effect)
}

test "capital D deletes from the cursor to the end of line" {
    expect step(at("abcd", 1), "D").state.lines == ["a"]
    expect step(at("ab", 2), "D").effect == Redraw
    expect isEdit(step(at("abcd", 1), "D").effect)
    // `D` stays in Normal — it is `C` that continues into Insert.
    expect step(at("abcd", 1), "D").state.mode == Normal
}

test "capital C changes to end of line and enters insert" {
    expect step(at("abcd", 1), "C").state.lines == ["a"]
    expect step(at("abcd", 1), "C").state.col == 1
    expect step(at("abcd", 1), "C").state.mode == Insert
    expect isEdit(step(at("abcd", 1), "C").effect)
}

test "s substitutes the char under the cursor and enters insert" {
    expect step(at("abc", 1), "s").state.lines == ["ac"]
    expect step(at("abc", 1), "s").state.col == 1
    expect step(at("abc", 1), "s").state.mode == Insert
    expect isEdit(step(at("abc", 1), "s").effect)
}

// `r` is the one two-keystroke command that gets its own *mode* rather than a
// pending field — a mini accumulator, so it has the same abandon/no-op duties as
// operator-pending does.
test "r replaces the char under the cursor via a two-key sequence" {
    expect step(initialState("abc"), "r").state.mode == Replace
    expect step(initialState("abc"), "r").effect == Redraw
    let pend = Editor(..initialState("abc"), col: 1, mode: Replace)
    expect step(pend, "Z").state.lines == ["aZc"]
    expect step(pend, "Z").state.col == 1
    expect step(pend, "Z").state.mode == Normal
    expect isEdit(step(pend, "Z").effect)
    // Esc cancels with no edit.
    expect step(pend, "Esc").state.mode == Normal
    expect step(pend, "Esc").effect == Redraw
    // At end of line there is nothing to replace, but the mode still resets.
    let atEnd = Editor(..initialState("abc"), col: 3, mode: Replace)
    expect step(atEnd, "Z").effect == Redraw
    expect step(atEnd, "Z").state.mode == Normal
}

test "step dispatches insert-mode keys" {
    let ins = Editor(..initialState("ac"), col: 1, mode: Insert)
    expect step(ins, "b").effect == Redraw
    expect step(ins, "b").state.lines == ["abc"]
    expect step(ins, "b").state.col == 2
    expect step(ins, "Esc").state.mode == Normal
    // A split keeps Insert mode.
    expect step(ins, "Enter").state.mode == Insert
    expect step(ins, "Backspace").state.lines == ["c"]
    expect step(ins, "Backspace").state.col == 0
}

// `:w` is the only path that produces a value the driver must act on, so the
// effect's *payload* is asserted, not just its tag.
test "step colon w saves the buffer and returns to normal" {
    let cmd = step(initialState("hi\nthere"), ":").state
    expect savedText(step(cmd, "w").effect) == "hi\nthere"
    expect step(cmd, "w").state.mode == Normal
    // `:q` quits; the driver breaks its loop on it.
    expect step(cmd, "q").effect == Quit
    expect step(cmd, "q").state.mode == Normal
    // Any other command key cancels harmlessly.
    expect step(cmd, "z").effect == Redraw
    expect step(cmd, "z").state.mode == Normal
}
