// spellcheck.st — suggest the closest dictionary word to a typo, by
// Levenshtein edit distance over a dynamic-programming table (the same
// shape as diff.st's LCS table: `fold` growing the table row by row, an
// inner `fold` growing each row cell by cell — insert/delete/substitute in
// place of diff's match/no-match recurrence).
//
// **The dictionary is `List<Str>`, not `Set<Str>` — because `Set` doesn't
// exist in this interpreter at all**, not even as a workaround-requiring
// stdlib gap. `plans/lang/01-grammar-and-precedence.md` documents the
// *design* ("`Set<T>` — eager; no literal, `[1,2,3] |> toSet`"), but there
// is no `Value::Set`, no `toSet` native, and no `Set` case in the checker
// — grepped `natives.rs`/`value.rs`/`check.rs` for any trace, found none.
// This is a step further than the `Map` finding (json.st's entry): `Map`
// is *built* but construction-limited; `Set` was never built past the
// design doc at all. `contains` (already a prelude function, `List`-only)
// covers everything this file needs from one, so the gap costs nothing
// here — but it would matter to a program that actually wanted set
// algebra (union/intersection/difference).

use Str
use List

chars(s: Str) -> List<Str> = Str.split(s, "") |> filter(c -> c != "")

min2(a: Int, b: Int) -> Int = a < b => a | b
min3(a: Int, b: Int, c: Int) -> Int = min2(min2(a, b), c)

lastRow(rows: List<List<Int>>) -> List<Int> = last(rows) |> unwrapOr([])
cellAt1(row: List<Int>, j: Int) -> Int = List.at(row, j) |> unwrapOr(0)
cellAt(table: List<List<Int>>, i: Int, j: Int) -> Int =
    List.at(table, i) |> mapMaybe(row -> cellAt1(row, j)) |> unwrapOr(0)

prod RowBuild(row: List<Int>, j: Int)

// `table[i][j]` = the edit distance between `a[0..i)` and `b[0..j)`.
// `d[i][0] = i` / `d[0][j] = j` (deleting/inserting everything) seeds row 0
// and each row's own column 0.
levTable(a: List<Str>, b: List<Str>) -> List<List<Int>> = {
    let firstRow = map(0..=count(b), i -> i) |> toList
    fold(enumerate(a), [firstRow], (rows, indexed) ->
        match indexed { (i, ai) => concat(rows, [levRow(lastRow(rows), i + 1, ai, b)]) })
}

levRow(prevRow: List<Int>, rowNum: Int, ai: Str, b: List<Str>) -> List<Int> =
    fold(b, RowBuild([rowNum], 1), (acc, bj) -> {
        let diag = cellAt1(prevRow, acc.j - 1)
        let up = cellAt1(prevRow, acc.j)
        let left = cellAt1(acc.row, acc.j - 1)
        let cell = ai == bj => diag | 1 + min3(diag, up, left)
        RowBuild(concat(acc.row, [cell]), acc.j + 1)
    }).row

levenshtein(a: Str, b: Str) -> Int = {
    let ca = chars(a)
    let cb = chars(b)
    cellAt(levTable(ca, cb), count(ca), count(cb))
}

// --- spellchecking ---

// `word` itself if it's already in the dictionary (no correction needed —
// and this also short-circuits the O(dictionary size) scan below for the
// common case of correctly-spelled input); otherwise the closest
// dictionary word by edit distance. Ties go to whichever candidate the
// dictionary lists first — `<`, not `<=`, in the fold below.
suggest(dict: List<Str>, word: Str) -> Maybe<Str> =
    contains(dict, word)
        => Some(word)
        | closest(dict, word)

prod Candidate(word: Str, distance: Int)

closest(dict: List<Str>, word: Str) -> Maybe<Str> =
    dict
        |> map(w -> Candidate(w, levenshtein(w, word)))
        |> fold(None, (best, c) -> match best {
            None => Some(c)
            Some(b) => c.distance < b.distance => Some(c) | best
        })
        |> mapMaybe(c -> c.word)

// Every dictionary word within `maxDistance`, nearest first.
suggestionsWithin(dict: List<Str>, word: Str, maxDistance: Int) -> List<Str> =
    dict
        |> filter(w -> levenshtein(w, word) <= maxDistance)
        |> sortBy(w -> levenshtein(w, word))

// --- tests ---

test "levenshtein matches the textbook kitten/sitting distance of 3" {
    expect levenshtein("kitten", "sitting") == 3
}

test "levenshtein against an empty string is just the other string's length" {
    expect levenshtein("", "abc") == 3
    expect levenshtein("abc", "") == 3
}

test "levenshtein of a string against itself is zero" {
    expect levenshtein("abc", "abc") == 0
}

test "levenshtein counts a single substitution as distance one" {
    expect levenshtein("cat", "cot") == 1
}

test "suggest returns the word itself when it's already in the dictionary" {
    let dict = ["hello", "world", "help"]
    expect suggest(dict, "hello") == Some("hello")
}

test "suggest finds the closest word for a typo, breaking ties by dictionary order" {
    let dict = ["hello", "world", "help"]
    // "helo" is edit-distance 1 from both "hello" and "help" — "hello"
    // comes first in the dictionary, so it wins the tie.
    expect suggest(dict, "helo") == Some("hello")
}

test "suggest against an empty dictionary is None" {
    expect suggest([], "anything") == None
}

test "suggestionsWithin lists every close-enough word, nearest first" {
    let dict = ["hello", "world", "help", "yell"]
    // "world" (distance 4) is excluded; the rest are all distance 1 from
    // "hell", so a stable sort leaves them in dictionary order.
    expect suggestionsWithin(dict, "hell", 2) == ["hello", "help", "yell"]
}

test "suggestionsWithin is empty when nothing in the dictionary is close enough" {
    let dict = ["hello", "world"]
    expect suggestionsWithin(dict, "xyz", 1) == []
}
