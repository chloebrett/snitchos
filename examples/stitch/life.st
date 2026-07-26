// life.st — Conway's Game of Life over `List<List<Bool>>`: a pure `step`,
// a text renderer, and a driver that emits one telemetry span *per
// generation* without blowing the interpreter's call-stack depth guard.
//
// That last part needs explaining, because the obvious way to write it
// doesn't work. `use <-` (`span("life.generation")  …`) desugars to the
// rest of the block becoming a *callback argument* to `span` — so a
// self-recursive "drive N generations" loop written as
// `n <= 0 => grid | { use <- span("life.generation")  loop(step(grid), n - 1) }`
// puts the recursive call *inside a lambda passed to a native*, not in the
// closure's own tail position. The trampoline (`eval_tail_dispatch` in
// `interp.rs`) only recognizes a direct self-call sitting in tail
// position; a call reached through a native's callback invocation doesn't
// qualify no matter how the AST reads. Confirmed empirically (a scratch
// `loop(n) uses Telemetry = n <= 0 => 0 | { use <- span("tick")
// loop(n - 1) }` faults with "call stack too deep" at ~48 iterations,
// `Env::MAX_CALL_DEPTH`) before writing this file's actual driver. See
// plans/stitch-examples-findings.md.
//
// The fix: don't drive the repetition with Stitch-level recursion at all.
// `Seq.iterate(grid, step)` is a *native* lazy sequence — forcing it loops
// in Rust, not through the Stitch call-stack depth counter — so
// `generations` below can produce however many generations are asked for
// with zero recursion-depth risk, and the per-generation telemetry is
// emitted afterward, once per already-computed generation, via `each`
// (also a native loop under the hood). Same span, same "one per tick" — a
// different loop underneath it.

use Str
use List
use Seq

// --- the model ---
//
// A grid is a plain `List<List<Bool>>` — there's no type-alias syntax in
// Stitch to name that shape (only `prod`/`sum`/`contract`), so every
// signature below just spells it out or leaves it untyped.

rows(grid) -> Int = count(grid)
cols(grid) -> Int = match first(grid) { Some(row) => count(row)  None => 0 }

// Off the edge of the grid reads as dead — the simplest boundary rule (no
// wraparound), and the one that makes a pattern like the blinker below
// behave exactly like the textbook version near a grid edge.
cellAt(grid, r: Int, c: Int) -> Bool =
    List.at(grid, r) |> mapMaybe(row -> List.at(row, c) |> unwrapOr(false)) |> unwrapOr(false)

liveNeighbors(grid, r: Int, c: Int) -> Int =
    [(0 - 1, 0 - 1), (0 - 1, 0), (0 - 1, 1), (0, 0 - 1), (0, 1), (1, 0 - 1), (1, 0), (1, 1)]
        |> map(d -> match d { (dr, dc) => cellAt(grid, r + dr, c + dc) => 1 | 0 })
        |> total

// The rule itself: a live cell survives on 2 or 3 neighbors (else dies, of
// under- or over-population); a dead cell is born on exactly 3.
nextCell(grid, r: Int, c: Int) -> Bool = {
    let n = liveNeighbors(grid, r, c)
    cellAt(grid, r, c) => (n == 2 or n == 3) | n == 3
}

// Pure: `grid` is untouched, a whole new grid comes back. Every cell's
// next state depends only on the *current* grid, computed independently —
// there's no "update in place and let later cells see earlier updates"
// hazard a mutable-array implementation has to guard against by copying.
step(grid) -> List<List<Bool>> =
    grid |> enumerate |> map(rowPair -> match rowPair { (r, row) =>
        row |> enumerate |> map(cellPair -> match cellPair { (c, _) => nextCell(grid, r, c) })
    })

countAlive(grid) -> Int = grid |> map(row -> row |> map(c -> c => 1 | 0) |> total) |> total

render(grid) -> Str =
    grid |> map(row -> row |> map(alive -> alive => "#" | ".") |> Str.join("")) |> Str.join("\n")

// --- driving generations ---

// `n + 1` because `iterate`'s first element is the seed itself (generation
// 0) — `generations(g, 0) == [g]`, not `[]`.
generations(grid, n: Int) -> List<List<List<Bool>>> =
    Seq.iterate(grid, step) |> take(n + 1) |> toList

// Emits one `life.generation` span per generation (including generation 0)
// carrying that generation's live-cell count, then returns the final grid.
run(grid, n: Int) -> List<List<Bool>> uses Telemetry = {
    let gens = generations(grid, n)
    each(gens, g -> {
        use <- span("life.generation")
        emit("life.alive_count", countAlive(g))
    })
    last(gens) |> unwrapOr(grid)
}

// --- tests ---

test "cellAt is false off every edge of the grid" {
    let grid = [[true, false], [false, true]]
    expect cellAt(grid, 0, 0) == true
    expect cellAt(grid, 0 - 1, 0) == false
    expect cellAt(grid, 0, 0 - 1) == false
    expect cellAt(grid, 5, 5) == false
}

test "liveNeighbors counts only in-bounds, non-self neighbors" {
    let grid = [[true, true, false], [true, false, false], [false, false, false]]
    expect liveNeighbors(grid, 1, 1) == 3
    expect liveNeighbors(grid, 0, 0) == 2
}

test "nextCell applies the birth/survival/death rule directly" {
    let grid = [[true, true, false], [true, false, false], [false, false, false]]
    // (1,1) is dead with 3 live neighbors -> born.
    expect nextCell(grid, 1, 1) == true
    // (0,0) is alive with 2 live neighbors -> survives.
    expect nextCell(grid, 0, 0) == true
    // (0,2) is dead with 1 live neighbor -> stays dead.
    expect nextCell(grid, 0, 2) == false
}

test "the blinker oscillates between horizontal and vertical every step" {
    let horizontal = [
        [false, false, false],
        [true, true, true],
        [false, false, false],
    ]
    let vertical = [
        [false, true, false],
        [false, true, false],
        [false, true, false],
    ]
    expect step(horizontal) == vertical
    expect step(vertical) == horizontal
    expect step(step(horizontal)) == horizontal
}

test "a block is a still life: step leaves it unchanged" {
    let block = [
        [false, false, false, false],
        [false, true, true, false],
        [false, true, true, false],
        [false, false, false, false],
    ]
    expect step(block) == block
}

test "an all-dead grid stays all-dead" {
    let empty = [[false, false], [false, false]]
    expect step(empty) == empty
}

test "countAlive sums live cells across every row" {
    expect countAlive([[true, false], [true, true]]) == 3
    expect countAlive([[false, false], [false, false]]) == 0
}

test "render draws # for alive and . for dead, rows joined by newline" {
    let grid = [[true, false, true], [false, true, false]]
    expect render(grid) == "#.#\n.#."
}

test "generations includes the seed as generation 0" {
    let horizontal = [[false, false, false], [true, true, true], [false, false, false]]
    let vertical = [[false, true, false], [false, true, false], [false, true, false]]
    expect generations(horizontal, 0) == [horizontal]
    expect generations(horizontal, 2) == [horizontal, vertical, horizontal]
}

test "run returns the final generation and emits telemetry along the way" uses Telemetry {
    let horizontal = [[false, false, false], [true, true, true], [false, false, false]]
    let vertical = [[false, true, false], [false, true, false], [false, true, false]]
    expect run(horizontal, 2) == horizontal
    expect run(horizontal, 1) == vertical
}
