// handlers.st — a `handle`/`with` demo whose actual point is the
// mechanism itself, not a new domain: a mock `Telemetry` handler that
// records every `emit` into an ordinary Stitch `List` instead of sending
// real frames, so `primes.st`-style code can be unit-tested purely in
// Stitch — no `FakePlatform`, no dropping to Rust, no real capability
// needed to *observe* what a function emitted (though the function under
// test still needs its own `uses Telemetry` to be *allowed* to call
// `emit` at all — `handle` changes where a permitted effect routes, not
// whether performing it is authorized; `natives.rs::native_emit`'s
// authority check runs regardless of any handler being installed).
//
// The capture works because a `mut` binding closed over by a lambda is
// captured *by reference* (`env.rs`'s `Scope` comment: "a `mut` binding
// reassigned here is visible through every clone of this scope —
// including closures that captured it"), the same mechanism bank.st's
// write-back relies on, applied here to let a handler accumulate into a
// variable declared *outside* the `handle` block.

prod EmitRecord(name: Str, value: Int)

// Run `work`, redirecting every `emit` it performs into a list instead of
// the real telemetry sink, and return what was recorded. `work` is called
// through a parameter, not a known top-level name, so the static checker's
// `uses`-propagation (shell.st's entry) doesn't — and structurally can't —
// require `captureEmits` itself to declare `Telemetry`; only `work` does,
// and `captureEmits` below genuinely has no `uses` row at all.
//
// The *tests*, though, all need `uses Telemetry` — not because they call
// `captureEmits` (whose own `uses` is empty), but because each one writes
// `() -> sumSquares(...)` inline: a lambda *literal* sitting directly in
// the test's own body, whose contents the checker's effect walker reaches
// through ordinary syntactic recursion (`child_exprs` descends into a
// lambda's body same as any other child), regardless of whether that
// lambda is invoked yet at that point in the source. Propagation isn't
// gated on "does this actually run" — it's "does this capability-needing
// call appear anywhere in the tree," lambdas included. See
// plans/stitch-examples-findings.md for the fuller writeup.
captureEmits(work) -> List<EmitRecord> = {
    let mut log = []
    handle emit with (n, v) -> { log = concat(log, [EmitRecord(n, v)]) } {
        work()
    }
    log
}

// --- representative "primes.st-style" code under test ---

sumSquares(xs: List<Int>) -> Int uses Telemetry {
    use <- span("sumSquares.compute")
    let result = xs |> map(x -> x * x) |> total
    emit("sumSquares.result", result)
    result
}

countEvens(xs: List<Int>) -> Int uses Telemetry {
    let evens = xs |> filter(x -> x % 2 == 0)
    emit("countEvens.count", count(evens))
    count(evens)
}

noEmit() uses Telemetry = ()

// --- tests ---

test "captureEmits records what the work under test emitted" uses Telemetry {
    expect captureEmits(() -> sumSquares([1, 2, 3])) == [EmitRecord("sumSquares.result", 14)]
}

test "captureEmits works for a second, differently-shaped emitter" uses Telemetry {
    expect captureEmits(() -> countEvens([1, 2, 3, 4])) == [EmitRecord("countEvens.count", 2)]
}

test "captureEmits is empty when the work under test never emits" uses Telemetry {
    expect captureEmits(() -> noEmit()) == []
}

test "each captureEmits call starts fresh — nothing leaks between calls" uses Telemetry {
    let first = captureEmits(() -> sumSquares([1, 2]))
    let second = captureEmits(() -> sumSquares([3, 4]))
    expect first == [EmitRecord("sumSquares.result", 5)]
    expect second == [EmitRecord("sumSquares.result", 25)]
}

test "captureEmits still returns the work's own value via the closure, independent of the log" uses Telemetry {
    // The point being demonstrated: capturing telemetry doesn't require
    // giving up the function's normal return value — `work` in these
    // tests always computes its answer the same way whether or not
    // anyone's watching what it emits.
    expect sumSquares([1, 2, 3]) == 14
    expect captureEmits(() -> sumSquares([1, 2, 3])) == [EmitRecord("sumSquares.result", 14)]
}
