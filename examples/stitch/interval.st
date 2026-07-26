// interval.st — two classic interval-scheduling algorithms: merging every
// overlapping range into its covering span, and picking the largest set
// of *non*-overlapping ranges (the activity-selection problem). Both are
// the textbook "sort, then greedy `fold`" shape — they only differ in
// *what* they sort by (start vs. end) and what the fold does on a
// conflict (extend vs. skip).

use List

prod Interval(start: Int, end: Int)

max2(a: Int, b: Int) -> Int = a > b => a | b

lastOf(xs: List<Interval>) -> Maybe<Interval> = last(xs)

dropLast(xs: List<Interval>) -> List<Interval> = List.removeAt(xs, count(xs) - 1)

// Sort by start; fold left to right, extending the running last interval
// whenever the next one starts at or before its end (touching counts as
// overlapping — `[1,4]` and `[4,5]` merge into `[1,5]`), otherwise
// starting a new run.
mergeIntervals(intervals: List<Interval>) -> List<Interval> =
    fold(sortBy(intervals, iv -> iv.start), [], (acc, iv) ->
        match lastOf(acc) {
            None => [iv]
            Some(prev) =>
                iv.start <= prev.end
                    => concat(dropLast(acc), [Interval(prev.start, max2(prev.end, iv.end))])
                    | concat(acc, [iv])
        })

// Sort by *end* (not start) and greedily take every interval that starts
// at or after the last one taken's end — the standard proof-has-a-name
// greedy-is-optimal algorithm: always keeping the option that frees up the
// earliest leaves the most room for what comes after.
maxNonOverlapping(intervals: List<Interval>) -> List<Interval> =
    fold(sortBy(intervals, iv -> iv.end), [], (acc, iv) ->
        match lastOf(acc) {
            None => concat(acc, [iv])
            Some(prev) => iv.start >= prev.end => concat(acc, [iv]) | acc
        })

// --- tests ---

test "mergeIntervals merges overlapping ranges, leaves disjoint ones alone" {
    let intervals = [Interval(1, 3), Interval(2, 6), Interval(8, 10), Interval(15, 18)]
    expect mergeIntervals(intervals) == [Interval(1, 6), Interval(8, 10), Interval(15, 18)]
}

test "mergeIntervals merges touching ranges (end equals next start)" {
    expect mergeIntervals([Interval(1, 4), Interval(4, 5)]) == [Interval(1, 5)]
}

test "mergeIntervals works regardless of input order" {
    let intervals = [Interval(15, 18), Interval(2, 6), Interval(1, 3), Interval(8, 10)]
    expect mergeIntervals(intervals) == [Interval(1, 6), Interval(8, 10), Interval(15, 18)]
}

test "mergeIntervals on non-overlapping input returns it unchanged, sorted" {
    expect mergeIntervals([Interval(5, 6), Interval(1, 2)]) == [Interval(1, 2), Interval(5, 6)]
}

test "mergeIntervals on a single interval is that interval" {
    expect mergeIntervals([Interval(1, 2)]) == [Interval(1, 2)]
}

test "mergeIntervals on no intervals is empty" {
    expect mergeIntervals([]) == []
}

test "maxNonOverlapping picks the largest compatible set, greedy by earliest end" {
    let intervals = [Interval(1, 3), Interval(2, 4), Interval(3, 5), Interval(6, 7), Interval(8, 9), Interval(8, 10)]
    expect maxNonOverlapping(intervals) == [Interval(1, 3), Interval(3, 5), Interval(6, 7), Interval(8, 9)]
}

test "maxNonOverlapping on fully overlapping intervals keeps only one" {
    expect maxNonOverlapping([Interval(1, 10), Interval(2, 9), Interval(3, 8)]) == [Interval(3, 8)]
}

test "maxNonOverlapping on already-disjoint intervals keeps them all" {
    expect maxNonOverlapping([Interval(1, 2), Interval(3, 4), Interval(5, 6)]) == [
        Interval(1, 2), Interval(3, 4), Interval(5, 6),
    ]
}
