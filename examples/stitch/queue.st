// queue.st — a producer/consumer queue depth simulation, driven the same
// way life.st's generation loop is: `Seq.iterate` + `take` + `toList`, a
// *native* loop, rather than hand-written Stitch recursion — the pattern
// life.st's entry in plans/stitch-examples-findings.md recommends for
// "N repetitions" once the `use <-`-breaks-the-trampoline finding landed.
//
// The proposal for this file named `stats.st`'s `summarise` as the
// throughput measure, via a cross-module `use stats.{summarise}`. That
// doesn't work standalone: this batch's programs each parse independently
// under `stitch/tests/examples.rs` (single-file `parse_program`, not the
// multi-module `run_modules` path `use` needs) — the same reason life.st's
// renderer is written inline rather than importing `fs-image/lib/text.st`.
// `summarise` is reimplemented locally instead, in miniature.

use Seq

prod Sim(queueLen: Int)

// One tick: up to `consumerRate` items are drained from whatever was
// *already* queued (never below zero — a consumer can't process work that
// hasn't arrived yet), and only then do this tick's `producerRate` new
// items arrive. Consume-before-produce, not the other way around: an item
// can't be consumed the same tick it arrives, so a steady state under
// matched rates settles at `producerRate`, not zero — the queue is always
// "last tick's arrivals, not yet touched."
step(sim: Sim, producerRate: Int, consumerRate: Int) -> Sim = {
    let consumed = sim.queueLen - consumerRate
    Sim((consumed < 0 => 0 | consumed) + producerRate)
}

// The queue length at every tick from 0 to `ticks`, inclusive (so
// `simulate(_, _, 0)` is the single starting length, matching life.st's
// `generations(g, 0) == [g]` convention).
simulate(producerRate: Int, consumerRate: Int, ticks: Int) -> List<Int> =
    Seq.iterate(Sim(0), s -> step(s, producerRate, consumerRate)) |> take(ticks + 1) |> toList |> map(s -> s.queueLen)

prod Summary(count: Int, total: Int, mean: Int, lowest: Int, highest: Int)

// A miniature `stats.st`-shaped summary — count, total, truncating mean,
// min, max — over the queue-length series a run produces.
summarise(xs: List<Int>) -> Summary = {
    let n = count(xs)
    n == 0
        => Summary(0, 0, 0, 0, 0)
        | Summary(n, total(xs), total(xs) / n, min(xs) |> unwrapOr(0), max(xs) |> unwrapOr(0))
}

// --- tests ---

test "a producer faster than the consumer grows the queue without bound" {
    let series = simulate(3, 1, 5)
    expect series == [0, 3, 5, 7, 9, 11]
}

test "a consumer faster than the producer settles at exactly one tick's arrivals" {
    // Never reaches zero and stays there — whatever arrives this tick
    // hasn't had a chance to be consumed yet, so the floor is
    // `producerRate`, not zero.
    let series = simulate(1, 3, 5)
    expect series == [0, 1, 1, 1, 1, 1]
}

test "matched rates hold the queue at a steady non-zero length" {
    let series = simulate(2, 2, 4)
    expect series == [0, 2, 2, 2, 2]
}

test "simulate with zero ticks is just the starting length" {
    expect simulate(5, 1, 0) == [0]
}

test "summarise reports count, total, mean, and range over a series" {
    let series = simulate(3, 1, 5)
    let s = summarise(series)
    expect s.count == 6
    expect s.total == 0 + 3 + 5 + 7 + 9 + 11
    expect s.mean == 35 / 6
    expect s.lowest == 0
    expect s.highest == 11
}

test "summarise on an empty series is all zeroes, not an error" {
    expect summarise([]) == Summary(0, 0, 0, 0, 0)
}
