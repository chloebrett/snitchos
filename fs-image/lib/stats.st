// stats.st — summary statistics over a list of Ints.
//
// The shape `analyze` in `primes.st` reaches for by hand, extracted so a
// telemetry consumer can compute it once and pass it around as a value:
// `use stats.{summarise}` then `summarise(readings)`.
//
// Integer arithmetic throughout, and `mean` truncates. That is a deliberate
// choice, not an oversight: these numbers describe tick counts and byte
// totals, where a fractional mean is noise dressed as precision.

use List

// Everything worth knowing about a list of numbers, computed in one pass over
// a sorted copy. A `prod` rather than five return values so a caller can hold
// the summary, compare two of them, or emit it field by field.
ext prod Summary(
    ext count: Int,
    ext total: Int,
    ext mean: Int,
    ext median: Int,
    ext lowest: Int,
    ext highest: Int
)

// Summarise `xs`. An empty list summarises to all-zeroes rather than a
// `Maybe<Summary>`: every field of the empty summary is the true answer for
// no data (nothing summed to zero, no spread), and forcing every caller
// through an unwrap to learn that buys nothing.
ext summarise(xs: List<Int>) -> Summary = {
    let n = count(xs)
    n == 0 => empty() | {
        let sorted = sort(xs)
        // `sum` is the `sum`-type keyword, so the total needs another name.
        let added = total(xs)
        Summary(
            count: n,
            total: added,
            mean: added / n,
            median: middle(sorted, n),
            lowest: at(sorted, 0),
            highest: at(sorted, n - 1)
        )
    }
}

test "summarise reports the whole shape of a list" {
    let s = summarise([3, 1, 4, 1, 5])
    expect s.count == 5
    expect s.total == 14
    expect s.lowest == 1
    expect s.highest == 5
}

// Truncation is the documented contract, so it is asserted rather than left to
// be discovered: 14/5 is 2, not 3.
test "mean truncates rather than rounding" {
    expect summarise([3, 1, 4, 1, 5]).mean == 2
}

// The summary of no data.
ext empty() -> Summary =
    Summary(count: 0, total: 0, mean: 0, median: 0, lowest: 0, highest: 0)

// The empty list is the case a summariser gets wrong by dividing by zero or by
// forcing every caller through a `Maybe`. Zeroes are the true answer.
test "the empty list summarises to zeroes" {
    let s = summarise([])
    expect s.count == 0
    expect s.mean == 0
    expect range(s) == 0
}

// One reading has no spread, and its median, mean, and both extremes are all
// that reading — the degenerate case every branch has to agree on.
test "a single reading is its own summary" {
    let s = summarise([7])
    expect s.median == 7
    expect s.mean == 7
    expect range(s) == 0
}

// How far the data spreads: highest minus lowest. Zero for a single reading,
// which is correct — one sample has no spread — and the reason this is a
// function on `Summary` rather than a seventh field.
ext range(s: Summary) -> Int = s.highest - s.lowest

test "range is the distance between the extremes" {
    expect range(summarise([3, 1, 4, 1, 5])) == 4
}

// The median of an already-sorted list of `n` elements. Even lengths take the
// mean of the two middle values, so the median of `[1, 2, 3, 4]` is 2, not 2.5
// — the same truncation as `mean`, applied consistently.
middle(sorted: List<Int>, n: Int) -> Int = {
    let half = n / 2
    n % 2 == 1
        => at(sorted, half)
        | (at(sorted, half - 1) + at(sorted, half)) / 2
}

// The median has to sort first — reading the middle of the *input* returns
// whatever happened to be there, which is right often enough to hide the bug.
test "median sorts before it picks the middle" {
    expect summarise([5, 1, 3]).median == 3
    expect summarise([9, 1, 2, 8]).median == 5
}

// Index into a list, treating an out-of-range index as zero. Only reached with
// indices this module computed from the list's own length, so the default is
// unreachable in practice and present so the module has no partial functions.
at(xs: List<Int>, i: Int) -> Int = List.at(xs, i) |> unwrapOr(0)
