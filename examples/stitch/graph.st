// graph.st — BFS, DFS, topological sort, and connected components over a
// directed graph, built entirely from `fold`/`map`/`filter`/`find` — no
// mutation anywhere, including during construction.
//
// The adjacency structure would be `Map<Str, List<Str>>` in a language with
// a growable map; here it's `List<AdjEntry>`, an association list, for the
// reason recorded at length in plans/stitch-examples-findings.md (json.st's
// entry): `Map` today is a fixed-arity literal plus read-only indexing, not
// a type you can build from a computed, variable-length sequence of pairs.
// `Edge`/`AdjEntry`/`Degree` are all named `prod`s rather than tuples for
// the same reason vm.st's `Popped` was — every one of them gets read by
// field name more than once, which a tuple + a hand-rolled `at2` can't do
// as clearly.
//
// `bfs`/`dfs`/`topoSort` are all written as a `worker(state...) -> result`
// tail-recursive loop over an explicit queue/stack/frontier — the same
// shape as vm.st's `step`, and deliberately so: this is the first program
// in the batch written *after* the `Env::bind` self-tail-call fix, so a
// graph with a few hundred nodes visited breadth-first no longer risks
// `MAX_CALL_DEPTH` the way it would have before that fix landed (every one
// of these loops binds a variable — `rest`, `newVisited`, `chosen` — ahead
// of its own tail call, exactly the shape that used to silently fall back
// to real Rust recursion).

use List

prod Edge(from: Str, to: Str)
prod AdjEntry(node: Str, neighbors: List<Str>)
prod Degree(node: Str, count: Int)
prod Graph(nodes: List<Str>, adj: List<AdjEntry>)

// --- construction ---

// Every node in `nodes` gets an (initially empty) entry up front, so an
// isolated node — reachable by nothing, reaching nothing — still shows up
// in `g.adj` and in every traversal that starts from it, rather than
// silently vanishing because it never appeared on either side of an edge.
buildGraph(nodes: List<Str>, edges: List<Edge>) -> Graph = {
    let empty = map(nodes, n -> AdjEntry(n, []))
    Graph(nodes, fold(edges, empty, (acc, e) -> addEdge(acc, e.from, e.to)))
}

// Undirected: every edge is added in both directions. `connectedComponents`
// below assumes this — component membership isn't meaningful on a directed
// graph without picking a convention for it, so this file doesn't try.
buildUndirectedGraph(nodes: List<Str>, edges: List<Edge>) -> Graph = {
    let empty = map(nodes, n -> AdjEntry(n, []))
    let adj = fold(edges, empty, (acc, e) -> addEdge(addEdge(acc, e.from, e.to), e.to, e.from))
    Graph(nodes, adj)
}

// Append `to` onto `from`'s neighbor list. A no-op if `from` isn't a known
// node (rather than an error) — the same "total, not partial" stance
// `stats.st`'s `at` takes, on the grounds that this module never manufactures
// an out-of-range `from` itself; only a caller handing in an edge whose
// endpoint wasn't in `nodes` could, and dropping it silently is at least as
// defensible as faulting on it here.
addEdge(adj: List<AdjEntry>, from: Str, to: Str) -> List<AdjEntry> =
    map(adj, entry -> entry.node == from => AdjEntry(from, concat(entry.neighbors, [to])) | entry)

// --- queries ---

adjOf(g: Graph, node: Str) -> List<Str> =
    find(g.adj, e -> e.node == node) |> mapMaybe(e -> e.neighbors) |> unwrapOr([])

// --- breadth-first search ---
//
// `bfs` returns every node reachable from `start`, in visitation order —
// `start` first. A plain worklist algorithm: `queue` is the frontier
// (append new neighbors at the *end* — FIFO, breadth-first), `visited`
// accumulates in reverse (cheapest end to grow a list from) and is flipped
// once at the end.

bfs(g: Graph, start: Str) -> List<Str> = bfsLoop(g, [start], [])

bfsLoop(g: Graph, queue: List<Str>, visited: List<Str>) -> List<Str> =
    match first(queue) {
        None => reverse(visited)
        Some(node) => {
            let rest = List.removeAt(queue, 0)
            contains(visited, node)
                => bfsLoop(g, rest, visited)
                | {
                    let seen = concat([node], visited)
                    let fresh = adjOf(g, node) |> filter(n -> not contains(seen, n) and not contains(rest, n))
                    bfsLoop(g, concat(rest, fresh), seen)
                }
        }
    }

// --- depth-first search ---
//
// Iterative, not recursive — a `List` used as a stack instead of a queue.
// The only difference from `bfs` above is *where* freshly-discovered
// neighbors join the worklist: the front (LIFO) instead of the back (FIFO).
// That one-line difference is the entire distinction between the two
// traversal orders; everything else is identical on purpose.

dfs(g: Graph, start: Str) -> List<Str> = dfsLoop(g, [start], [])

dfsLoop(g: Graph, stack: List<Str>, visited: List<Str>) -> List<Str> =
    match first(stack) {
        None => reverse(visited)
        Some(node) => {
            let rest = List.removeAt(stack, 0)
            contains(visited, node)
                => dfsLoop(g, rest, visited)
                | {
                    let seen = concat([node], visited)
                    let fresh = adjOf(g, node) |> filter(n -> not contains(seen, n))
                    dfsLoop(g, concat(fresh, rest), seen)
                }
        }
    }

reachable(g: Graph, from: Str, to: Str) -> Bool = contains(bfs(g, from), to)

// --- topological sort (Kahn's algorithm) ---
//
// Repeatedly remove a node with no remaining incoming edge, decrementing
// its former neighbors' in-degree as it goes. If every node was eventually
// removed, the order removed is a valid topological order; if nodes remain
// once no zero-in-degree node is left, the graph has a cycle — Kahn's
// algorithm doubles as a cycle detector for free, which a plain DFS-based
// sort doesn't hand you as directly.

topoSort(g: Graph) -> Result<List<Str>, Str> = topoLoop(g, initialDegrees(g), [])

initialDegrees(g: Graph) -> List<Degree> = map(g.nodes, n -> Degree(n, incomingCount(g, n)))

incomingCount(g: Graph, node: Str) -> Int =
    g.adj |> map(e -> contains(e.neighbors, node) => 1 | 0) |> total

topoLoop(g: Graph, degrees: List<Degree>, acc: List<Str>) -> Result<List<Str>, Str> = {
    let zeros = filter(degrees, d -> d.count == 0)
    match first(zeros) {
        None => count(degrees) == 0 => Ok(reverse(acc)) | Err("cycle detected")
        Some(chosen) => {
            let remaining = filter(degrees, d -> d.node != chosen.node)
            let removedNeighbors = adjOf(g, chosen.node)
            let updated = map(remaining, d ->
                contains(removedNeighbors, d.node) => Degree(d.node, d.count - 1) | d)
            topoLoop(g, updated, concat([chosen.node], acc))
        }
    }
}

// --- connected components (undirected) ---
//
// Walk `g.nodes` in order; whenever a node hasn't shown up in any component
// found so far, `bfs` from it names the whole component in one pass. Every
// component is disjoint by construction (BFS from an already-visited node
// would just re-find the same set), so "already in some earlier component"
// is exactly the skip condition, no separate visited-set needed.

connectedComponents(g: Graph) -> List<List<Str>> = componentsLoop(g, g.nodes, [])

componentsLoop(g: Graph, remaining: List<Str>, acc: List<List<Str>>) -> List<List<Str>> =
    match first(remaining) {
        None => reverse(acc)
        Some(n) => {
            let rest = List.removeAt(remaining, 0)
            contains(flatten(acc), n)
                => componentsLoop(g, rest, acc)
                | componentsLoop(g, rest, concat([bfs(g, n)], acc))
        }
    }

// --- tests ---

test "buildGraph seeds every named node, even one with no edges" {
    let g = buildGraph(["a", "b", "isolated"], [Edge("a", "b")])
    expect adjOf(g, "a") == ["b"]
    expect adjOf(g, "b") == []
    expect adjOf(g, "isolated") == []
}

test "adjOf on an unknown node is empty, not an error" {
    let g = buildGraph(["a"], [])
    expect adjOf(g, "ghost") == []
}

test "bfs visits in breadth-first order and starts with the source" {
    // a -> b, a -> c, b -> d, c -> d : bfs(a) should reach d exactly once,
    // via whichever of b/c queued first, never revisiting it through the other.
    let g = buildGraph(["a", "b", "c", "d"], [Edge("a", "b"), Edge("a", "c"), Edge("b", "d"), Edge("c", "d")])
    expect bfs(g, "a") == ["a", "b", "c", "d"]
}

test "dfs visits depth-first, diverging from bfs's order on the same graph" {
    let g = buildGraph(["a", "b", "c", "d"], [Edge("a", "b"), Edge("a", "c"), Edge("b", "d"), Edge("c", "d")])
    // dfs pushes b's neighbors (d) ahead of c, so it reaches d before c.
    expect dfs(g, "a") == ["a", "b", "d", "c"]
}

test "bfs and dfs from an isolated node visit only that node" {
    let g = buildGraph(["a", "lonely"], [Edge("a", "a")])
    expect bfs(g, "lonely") == ["lonely"]
    expect dfs(g, "lonely") == ["lonely"]
}

test "bfs handles a cycle without looping forever" {
    let g = buildGraph(["a", "b", "c"], [Edge("a", "b"), Edge("b", "c"), Edge("c", "a")])
    expect bfs(g, "a") == ["a", "b", "c"]
}

test "reachable is true within a component and false across components" {
    let g = buildGraph(["a", "b", "c"], [Edge("a", "b")])
    expect reachable(g, "a", "b") == true
    expect reachable(g, "a", "c") == false
    expect reachable(g, "b", "a") == false
}

test "topoSort orders a DAG so every edge points forward" {
    let g = buildGraph(["a", "b", "c", "d"], [Edge("a", "b"), Edge("a", "c"), Edge("b", "d"), Edge("c", "d")])
    expect topoSort(g) == Ok(["a", "b", "c", "d"])
}

test "topoSort reports a cycle by name rather than looping" {
    let g = buildGraph(["x", "y"], [Edge("x", "y"), Edge("y", "x")])
    expect topoSort(g) == Err("cycle detected")
}

test "topoSort on a graph with no edges returns the nodes in input order" {
    let g = buildGraph(["z", "a", "m"], [])
    expect topoSort(g) == Ok(["z", "a", "m"])
}

test "connectedComponents groups an undirected graph into its disjoint pieces" {
    let g = buildUndirectedGraph(
        ["a", "b", "c", "d", "e"],
        [Edge("a", "b"), Edge("b", "c"), Edge("d", "e")],
    )
    expect connectedComponents(g) == [["a", "b", "c"], ["d", "e"]]
}

test "connectedComponents gives every isolated node its own singleton component" {
    let g = buildUndirectedGraph(["a", "b"], [])
    expect connectedComponents(g) == [["a"], ["b"]]
}

test "buildUndirectedGraph adds edges in both directions" {
    let g = buildUndirectedGraph(["a", "b"], [Edge("a", "b")])
    expect adjOf(g, "a") == ["b"]
    expect adjOf(g, "b") == ["a"]
}
