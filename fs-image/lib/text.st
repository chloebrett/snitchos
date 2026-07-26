// text.st — the string utilities every other program here kept re-writing.
//
// Load with `:load lib/text.st`, or import the pieces you want:
// `use text.{pad, wrap, indent}`. Everything is pure — no capabilities, no
// telemetry — so these are safe to call from anywhere, including inside a
// `without Telemetry` block.

use Str
use List

// Pad `s` on the right with spaces until it is `width` wide. A string already
// at or past `width` comes back untouched, so this never truncates: a table
// column that overflows should look wrong, not lie.
ext pad(s: Str, width: Int) -> Str = {
    let short = width - Str.length(s)
    short <= 0 => s | s + spaces(short)
}

// Pad on the left instead — what numbers want, so a column of them lines up on
// the ones digit.
ext padLeft(s: Str, width: Int) -> Str = {
    let short = width - Str.length(s)
    short <= 0 => s | spaces(short) + s
}

test "pad widens to the column and never truncates" {
    expect pad("ok", 5) == "ok   "
    expect padLeft("7", 3) == "  7"
    // A value wider than its column must look wrong, not be cut.
    expect pad("overlong", 3) == "overlong"
}

// `n` spaces. Built by folding rather than recursing so a wide pad costs one
// pass and no stack.
ext spaces(n: Int) -> Str = repeatStr(" ", n)

// `s` repeated `n` times. `n <= 0` is the empty string, not an error — callers
// are computing widths, and a negative width means "nothing to add".
ext repeatStr(s: Str, n: Int) -> Str =
    fold(1..=n, "", (acc, _) -> acc + s)

test "repeat of a non-positive count is empty rather than an error" {
    expect repeatStr("ab", 0) == ""
    expect repeatStr("ab", -3) == ""
}

// Split `s` into lines of at most `width` characters, breaking at spaces.
// A word longer than `width` gets its own line rather than being cut in half —
// truncating a token is how log output starts lying about identifiers.
ext wrap(s: Str, width: Int) -> List<Str> =
    Str.split(s, " ")
        |> fold([], (lines, word) -> addWord(lines, word, width))
        |> reverse

test "wrap breaks at spaces and keeps every word" {
    expect Str.join(wrap("the quick brown fox", 9), "|") == "the quick|brown fox"
}

// The case a naive `wrap` gets wrong: a single word longer than the width has
// nowhere to break, and cutting it would corrupt an identifier.
test "wrap gives an overlong word its own line intact" {
    expect Str.join(wrap("a supercalifragilistic b", 5), "|") == "a|supercalifragilistic|b"
}

// Prefix every line of `s` with `n` spaces. Blank lines stay blank — trailing
// whitespace on an empty line is invisible in a diff and noisy in a corpus.
ext indent(s: Str, n: Int) -> Str =
    Str.split(s, "\n")
        |> map(line -> line == "" => line | spaces(n) + line)
        |> Str.join("\n")

test "indent prefixes content lines and leaves blank ones alone" {
    expect indent("one\n\ntwo", 2) == "  one\n\n  two"
}

// Capitalise the first character, lowercase the rest: `"BOOT" -> "Boot"`.
ext titleCase(s: Str) -> Str = {
    Str.length(s) == 0 => s | {
        let head = Str.upper(Str.slice(s, 0, 1))
        let tail = Str.lower(Str.slice(s, 1, Str.length(s)))
        head + tail
    }
}

test "title case normalises the whole word" {
    expect titleCase("BOOT") == "Boot"
    expect titleCase("") == ""
}

// One step of `wrap`: put `word` on the current line if it fits, else start a
// new one. `lines` is built in reverse (newest first) so appending is a cons.
addWord(lines: List<Str>, word: Str, width: Int) -> List<Str> = {
    let current = lines |> first |> unwrapOr("")
    let joined = current + " " + word
    match {
        count(lines) == 0 => [word]
        // `drop` is a Seq operation; a List drops its head by index.
        Str.length(joined) <= width => concat([joined], List.removeAt(lines, 0))
        _ => concat([word], lines)
    }
}
