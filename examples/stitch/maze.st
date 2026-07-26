// maze.st — BFS shortest-path finding over a wall grid, with coordinates
// as plain `(Int, Int)` tuples rather than a named `prod`. A deliberate
// exception to the "prefer a named product once a value is read more than
// once" habit vm.st's entry established (plans/stitch-examples-findings.md):
// `(row, col)` is such a universally-understood positional pair — matching
// how you'd write it on paper, or in Python — that naming its fields
// (`Pos(row, col)`) would add ceremony without adding clarity, unlike
// `Popped(value, rest)`, where the field names *are* the documentation.

use List

// `true` = wall, `false` = open. Off the edge of the grid also reads as a
// wall — the same "boundary reads as the safe/blocked default" choice
// life.st's `cellAt` makes for "off the edge reads as dead."
wallAt(grid: List<List<Bool>>, pos: (Int, Int)) -> Bool =
    match pos { (r, c) => List.at(grid, r) |> mapMaybe(row -> List.at(row, c) |> unwrapOr(true)) |> unwrapOr(true) }

neighbors(pos: (Int, Int)) -> List<(Int, Int)> =
    match pos { (r, c) => [(r - 1, c), (r + 1, c), (r, c - 1), (r, c + 1)] }

prod CameFrom(pos: (Int, Int), from: (Int, Int))

// Walk `parents` backward from `node` to whichever ancestor has no entry
// (that's `start` — the search never records a parent for its own origin).
// Not tail-recursive (there's work — the `concat` — after the recursive
// call), but a maze's path length is bounded by the grid itself, so the
// depth here is nowhere near a concern the way an unbounded loop would be.
reconstructPath(parents: List<CameFrom>, node: (Int, Int)) -> List<(Int, Int)> =
    match find(parents, p -> p.pos == node) {
        None => [node]
        Some(cf) => concat(reconstructPath(parents, cf.from), [node])
    }

// Standard BFS worklist (the same shape as graph.st's `bfsLoop`): `queue`
// is the frontier, `visited` is marked at *discovery* time (not dequeue
// time) so a node already queued is never queued twice, and `parents`
// records the first-discovered edge into each node — which, because BFS
// discovers nodes in order of distance, is guaranteed to be a shortest-path
// edge.
findPath(grid: List<List<Bool>>, start: (Int, Int), goal: (Int, Int)) -> Maybe<List<(Int, Int)>> =
    bfsLoop(grid, [start], [start], [], goal)

bfsLoop(grid: List<List<Bool>>, queue: List<(Int, Int)>, visited: List<(Int, Int)>, parents: List<CameFrom>, goal: (Int, Int)) -> Maybe<List<(Int, Int)>> =
    match first(queue) {
        None => None
        Some(pos) =>
            pos == goal
                => Some(reconstructPath(parents, goal))
                | {
                    let rest = List.removeAt(queue, 0)
                    let fresh = neighbors(pos) |> filter(n -> not wallAt(grid, n) and not contains(visited, n) and not contains(rest, n))
                    let newVisited = concat(visited, fresh)
                    let newParents = concat(parents, map(fresh, n -> CameFrom(n, pos)))
                    bfsLoop(grid, concat(rest, fresh), newVisited, newParents, goal)
                }
    }

// --- tests ---

test "wallAt reads in-bounds cells directly and treats out-of-bounds as a wall" {
    let grid = [[false, true], [false, false]]
    expect wallAt(grid, (0, 0)) == false
    expect wallAt(grid, (0, 1)) == true
    expect wallAt(grid, (0 - 1, 0)) == true
    expect wallAt(grid, (5, 5)) == true
}

test "findPath when start equals goal is the single-cell path" {
    let grid = [[false, false], [false, false]]
    expect findPath(grid, (0, 0), (0, 0)) == Some([(0, 0)])
}

test "findPath finds a shortest route around an obstacle" {
    let grid = [
        [false, false, false],
        [true, true, false],
        [false, false, false],
    ]
    expect findPath(grid, (0, 0), (2, 0)) == Some([
        (0, 0), (0, 1), (0, 2), (1, 2), (2, 2), (2, 1), (2, 0),
    ])
}

test "findPath returns None when no route exists" {
    let grid = [
        [false, true, false],
        [false, true, false],
        [false, true, false],
    ]
    expect findPath(grid, (0, 0), (0, 2)) == None
}

test "findPath through a winding corridor returns the whole shortest path" {
    let grid = [
        [false, false, false, false, false],
        [false, true, true, true, false],
        [false, true, false, false, false],
        [false, true, false, true, true],
        [false, false, false, false, false],
    ]
    expect findPath(grid, (0, 0), (4, 4)) == Some([
        (0, 0), (1, 0), (2, 0), (3, 0), (4, 0),
        (4, 1), (4, 2), (4, 3), (4, 4),
    ])
}
