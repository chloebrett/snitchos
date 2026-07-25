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

// The summary of no data.
ext empty() -> Summary =
    Summary(count: 0, total: 0, mean: 0, median: 0, lowest: 0, highest: 0)

// How far the data spreads: highest minus lowest. Zero for a single reading,
// which is correct — one sample has no spread — and the reason this is a
// function on `Summary` rather than a seventh field.
ext range(s: Summary) -> Int = s.highest - s.lowest

// The median of an already-sorted list of `n` elements. Even lengths take the
// mean of the two middle values, so the median of `[1, 2, 3, 4]` is 2, not 2.5
// — the same truncation as `mean`, applied consistently.
middle(sorted: List<Int>, n: Int) -> Int = {
    let half = n / 2
    n % 2 == 1
        => at(sorted, half)
        | (at(sorted, half - 1) + at(sorted, half)) / 2
}

// Index into a list, treating an out-of-range index as zero. Only reached with
// indices this module computed from the list's own length, so the default is
// unreachable in practice and present so the module has no partial functions.
at(xs: List<Int>, i: Int) -> Int = List.at(xs, i) |> unwrapOr(0)
