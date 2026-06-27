// primes.st — instrumented number-crunching: the worked example for loading a
// Stitch program off the SnitchOS filesystem and watching it narrate itself.
//
// Load it at the prompt with `:load primes.st`, then drive it with your own
// input — `primes(25)` or `analyze([3, 1, 4, 1, 5, 9, 2, 6])`. Every call opens
// a real span and emits gauges, so you can watch the numbers move in Grafana as
// you type.

// Trial division: n is prime iff nothing in 2..n divides it. Deliberately the
// naive O(n) check — simple to read, and enough work to show up in a trace.
isPrime(n: Int) -> Bool =
    n > 1 and (2..n |> all(d -> n % d != 0))

// The first `n` primes, as a List. The whole search is one `primes.compute`
// span; it reports how many it found and the largest.
primes(n: Int) -> List<Int> uses Telemetry {
    use <- span("primes.compute")
    let found = 1.. |> filter(isPrime) |> take(n) |> toList
    emit("primes.count", found |> count)
    emit("primes.largest", found |> max |> unwrapOr(0))
    found
}

// Summary statistics over a list you supply. Spans the work, emits each stat as
// its own gauge, and returns the sum.
analyze(xs: List<Int>) -> Int uses Telemetry {
    use <- span("analyze")
    emit("analyze.count", xs |> count)
    emit("analyze.sum", xs |> total)
    emit("analyze.max", xs |> max |> unwrapOr(0))
    emit("analyze.min", xs |> min |> unwrapOr(0))
    xs |> total
}
