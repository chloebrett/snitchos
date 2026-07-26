// diff.st — a line-level diff between two `List<Str>`, built the textbook
// way: a longest-common-subsequence dynamic-programming table, then a
// backtrack over it that emits `Same`/`Added`/`Removed` lines. The DP
// table itself is the interesting part to write without mutable arrays —
// each row only depends on the row above it and the cells built so far in
// the row currently being built, so it's two nested `fold`s: one growing
// the table row by row, one growing each row cell by cell.

use List

sum DiffLine = Same(Str) | Added(Str) | Removed(Str)

// `table[i][j]` = the LCS length of `a[0..i)` and `b[0..j)`. Row 0 and
// column 0 (the "compared against nothing" cases) are 0 by definition —
// `firstRow` seeds row 0; every `nextRow` seeds its own column 0 with `0`
// the same way.
lcsTable(a: List<Str>, b: List<Str>) -> List<List<Int>> = {
    // `0..=count(b)` is a `Seq` (ranges are lazy — see docs/language-design.md),
    // and `map` over a `Seq` stays a `Seq` even though the source is finite;
    // `toList` is what actually forces it, since `List.at`/`concat`/etc.
    // below all need a real `List`.
    let firstRow = map(0..=count(b), _ -> 0) |> toList
    fold(a, [firstRow], (rows, ai) -> concat(rows, [nextRow(lastRow(rows), ai, b)]))
}

lastRow(rows: List<List<Int>>) -> List<Int> = last(rows) |> unwrapOr([])

prod RowBuild(row: List<Int>, j: Int)

// One row of the table, given the row above it (`prevRow`) and the
// source-`a` element this row is for (`ai`). `acc.row` already holds
// `row[0..j)` by the time cell `j` is computed, so `left` (`row[j-1]`)
// reads from the row under construction, not `prevRow`.
nextRow(prevRow: List<Int>, ai: Str, b: List<Str>) -> List<Int> =
    fold(b, RowBuild([0], 1), (acc, bj) -> {
        let diag = cellAt1(prevRow, acc.j - 1)
        let up = cellAt1(prevRow, acc.j)
        let left = cellAt1(acc.row, acc.j - 1)
        let cell = ai == bj => diag + 1 | (up > left => up | left)
        RowBuild(concat(acc.row, [cell]), acc.j + 1)
    }).row

cellAt1(row: List<Int>, j: Int) -> Int = List.at(row, j) |> unwrapOr(0)

cellAt(table: List<List<Int>>, i: Int, j: Int) -> Int =
    List.at(table, i) |> mapMaybe(row -> cellAt1(row, j)) |> unwrapOr(0)

lineAt(lines: List<Str>, i: Int) -> Str = List.at(lines, i) |> unwrapOr("")

// Walk from the bottom-right corner `(count(a), count(b))` back to
// `(0, 0)`: a matching line at both frontiers is `Same` (diagonal step);
// otherwise step toward whichever neighbor cell (left or up) carries the
// LCS this position's choice came from, emitting `Added` for a leftward
// step and `Removed` for an upward one. Self-tail-recursive, so the walk
// costs no Rust call-stack depth regardless of how long the inputs are
// (see vm.st's/life.st's entries in plans/stitch-examples-findings.md for
// why that isn't automatic).
backtrack(a: List<Str>, b: List<Str>, table: List<List<Int>>, i: Int, j: Int, acc: List<DiffLine>) -> List<DiffLine> =
    (i == 0 and j == 0)
        => acc
        | ((i > 0 and j > 0 and lineAt(a, i - 1) == lineAt(b, j - 1))
            => backtrack(a, b, table, i - 1, j - 1, concat([Same(lineAt(a, i - 1))], acc))
            | ((j > 0 and (i == 0 or cellAt(table, i, j - 1) >= cellAt(table, i - 1, j)))
                => backtrack(a, b, table, i, j - 1, concat([Added(lineAt(b, j - 1))], acc))
                | backtrack(a, b, table, i - 1, j, concat([Removed(lineAt(a, i - 1))], acc))))

diffLines(a: List<Str>, b: List<Str>) -> List<DiffLine> =
    backtrack(a, b, lcsTable(a, b), count(a), count(b), [])

// --- tests ---

test "identical inputs are all Same lines" {
    expect diffLines(["a", "b", "c"], ["a", "b", "c"]) == [Same("a"), Same("b"), Same("c")]
}

test "a removed-then-restored line shows as Removed between two Same lines" {
    expect diffLines(["a", "b", "c"], ["a", "c"]) == [Same("a"), Removed("b"), Same("c")]
}

test "an empty original against a nonempty new text is all Added" {
    expect diffLines([], ["x", "y"]) == [Added("x"), Added("y")]
}

test "a nonempty original against an empty new text is all Removed" {
    expect diffLines(["x", "y"], []) == [Removed("x"), Removed("y")]
}

test "two empty inputs diff to nothing" {
    expect diffLines([], []) == []
}

test "a reordering with one common line shows as remove-then-add around it" {
    expect diffLines(["a", "b"], ["b", "a"]) == [Removed("a"), Same("b"), Added("a")]
}

test "lcsTable's bottom-right cell is the LCS length of the whole inputs" {
    let table = lcsTable(["a", "b", "c"], ["a", "c"])
    expect cellAt(table, 3, 2) == 2
}
