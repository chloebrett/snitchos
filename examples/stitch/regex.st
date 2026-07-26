// regex.st — the smallest regex matcher (in the spirit of Kernighan's
// classic 30-line C version): literal characters, `.` (any char), `*`
// (zero-or-more of the preceding atom), `^`/`$` anchors, and top-level `|`
// alternation. No groups, no parens in the pattern language, no captures —
// existence matching only (`matches(pattern, text) -> Bool`), which is
// what keeps this a page long instead of a real regex engine.
//
// Batch 2 opens with a program that leans entirely on habits from batch 1:
// association-list-free (no dictionary needed here at all), named
// `prod`-free too (nothing here is read twice by field, so there was
// nothing to name), and every nested conditional whose else-branch is
// itself conditional gets explicit parens from the first draft — see
// json.st's entry in plans/stitch-examples-findings.md for why that's load
// -bearing, not stylistic.

use Str

head(s: Str) -> Str = Str.slice(s, 0, 1)
tail(s: Str) -> Str = Str.slice(s, 1, Str.length(s))
isEmpty(s: Str) -> Bool = Str.length(s) == 0

// Top-level entry: split on `|` (safe with no grouping construct to worry
// about — a literal `|` in the pattern is always an alternation boundary,
// never nested inside anything), and the pattern matches if any
// alternative does.
matches(pattern: Str, text: Str) -> Bool =
    Str.split(pattern, "|") |> any(alt -> matchAlternative(alt, text))

// `^` anchors the whole alternative to the start of `text`; otherwise the
// pattern may match starting anywhere in `text` (an unanchored regex is
// really "anchored, tried at every starting position").
matchAlternative(pattern: Str, text: Str) -> Bool =
    Str.startsWith(pattern, "^")
        => matchHere(tail(pattern), text)
        | searchFrom(pattern, text)

searchFrom(pattern: Str, text: Str) -> Bool =
    matchHere(pattern, text)
        => true
        | (isEmpty(text) => false | searchFrom(pattern, tail(text)))

// The core recursive matcher: does `pattern` match a *prefix* of `text`
// (not necessarily all of it — that's what a trailing `$` asserts)?
matchHere(pattern: Str, text: Str) -> Bool =
    isEmpty(pattern)
        => true
        | (pattern == "$"
            => isEmpty(text)
            | (Str.length(pattern) >= 2 and head(tail(pattern)) == "*"
                => matchStar(head(pattern), tail(tail(pattern)), text)
                | (not isEmpty(text) and (head(pattern) == "." or head(pattern) == head(text))
                    => matchHere(tail(pattern), tail(text))
                    | false)))

// `c*rest` against `text`: try the zero-repeat case first (does `rest`
// alone match here?), and only if that fails, consume one more `c` (or any
// char, if `c` is `.`) from `text` and try again. Shortest-match-first —
// fine for a yes/no matcher, where any successful backtrack proves a match
// exists; a captures-returning engine would need to think harder about
// which count to prefer.
matchStar(c: Str, pattern: Str, text: Str) -> Bool =
    matchHere(pattern, text)
        => true
        | (isEmpty(text)
            => false
            | ((c == "." or head(text) == c)
                => matchStar(c, pattern, tail(text))
                | false))

// --- tests ---

test "literal patterns match exactly and reject a mismatch" {
    expect matches("cat", "cat") == true
    expect matches("cat", "dog") == false
}

test "an unanchored pattern matches anywhere in the text" {
    expect matches("cat", "concatenate") == true
    expect matches("cat", "the cat sat") == true
    expect matches("cat", "no match here") == false
}

test "^ anchors the match to the start of the text" {
    expect matches("^cat", "cats") == true
    expect matches("^cat", "concat") == false
}

test "$ anchors the match to the end of the text" {
    expect matches("cat$", "concat") == true
    expect matches("cat$", "cats") == false
}

test "^ and $ together require an exact whole-text match" {
    expect matches("^cat$", "cat") == true
    expect matches("^cat$", "cats") == false
    expect matches("^cat$", "concat") == false
}

test "an empty pattern matches any text, including empty" {
    expect matches("", "anything") == true
    expect matches("", "") == true
}

test ". matches any single character" {
    expect matches("c.t", "cat") == true
    expect matches("c.t", "cot") == true
    expect matches("c.t", "ct") == false
    expect matches("c.t", "caat") == false
}

test "* matches zero or more of the preceding atom" {
    expect matches("^ab*c$", "ac") == true
    expect matches("^ab*c$", "abc") == true
    expect matches("^ab*c$", "abbbbc") == true
    expect matches("^ab*c$", "adc") == false
}

test ".* matches any run of characters, including none" {
    expect matches("^a.*b$", "ab") == true
    expect matches("^a.*b$", "aXYZb") == true
    expect matches("^.*$", "anything at all") == true
}

test "| tries each alternative and succeeds if any one matches" {
    expect matches("cat|dog", "I have a dog") == true
    expect matches("cat|dog", "I have a cat") == true
    expect matches("cat|dog", "I have a fish") == false
}

test "anchors apply per-alternative, not to the whole pattern" {
    // "^cat" is anchored, "fish$" is anchored at the other end — each
    // alternative carries its own anchoring.
    expect matches("^cat|fish$", "catfood") == true
    expect matches("^cat|fish$", "swordfish") == true
    expect matches("^cat|fish$", "a wildcat") == false
}
