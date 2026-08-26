import { describe, expect, it } from "vitest";
import { type Graph, type GraphNode, toForest } from "./graph";

const node = (id: string, over: Partial<GraphNode> = {}): GraphNode => ({
  id,
  label: id.toUpperCase(),
  classes: [],
  group: null,
  ...over,
});

const graph = (nodes: GraphNode[], edges: Graph["edges"] = []): Graph => ({
  direction: "LR",
  nodes,
  edges,
});

/** Every id in the forest, in the order it appears. */
function walk(forest: ReturnType<typeof toForest>): string[] {
  return forest.flatMap((t) => [t.node.id, ...walk(t.children)]);
}

describe("toForest", () => {
  it("roots the forest at nodes nothing points at", () => {
    const forest = toForest(
      graph([node("a"), node("b")], [{ from: "a", to: "b", label: null }]),
    );

    expect(forest).toHaveLength(1);
    expect(forest[0]?.node.id).toBe("a");
    expect(forest[0]?.children[0]?.node.id).toBe("b");
  });

  it("carries the edge label to the child it led to", () => {
    const forest = toForest(
      graph([node("a"), node("b")], [{ from: "a", to: "b", label: "SEND" }]),
    );
    expect(forest[0]?.children[0]?.via).toBe("SEND");
  });

  it("handles several roots", () => {
    const forest = toForest(graph([node("a"), node("b")]));
    expect(walk(forest)).toEqual(["a", "b"]);
  });

  /**
   * **The hazard this function exists for.** The switch-transition fold produces
   * cycles as a matter of course — task A yields to B yields back to A — and a naive
   * descent would recurse until the tab died.
   */
  it("terminates on a cycle", () => {
    const forest = toForest(
      graph(
        [node("a"), node("b")],
        [
          { from: "a", to: "b", label: null },
          { from: "b", to: "a", label: null },
        ],
      ),
    );

    // Nothing is un-pointed-at, so `a` becomes a root by the leftover rule.
    expect(walk(forest)).toEqual(["a", "b", "a"]);
    // ...and the second `a` is a reference, not an expansion.
    expect(forest[0]?.children[0]?.children[0]?.revisited).toBe(true);
  });

  /** A node reachable by two paths is expanded once and referenced after. */
  it("expands a shared node once", () => {
    const forest = toForest(
      graph(
        [node("root"), node("a"), node("b"), node("shared")],
        [
          { from: "root", to: "a", label: null },
          { from: "root", to: "b", label: null },
          { from: "a", to: "shared", label: null },
          { from: "b", to: "shared", label: null },
        ],
      ),
    );

    const shared = walk(forest).filter((id) => id === "shared");
    expect(shared).toHaveLength(2);
    expect(forest[0]?.children[1]?.children[0]?.revisited).toBe(true);
  });

  /**
   * A node that exists must be visible somewhere. Dropping one would make the panel
   * quietly disagree with the graph it was handed — the same class of silent lie the
   * frame-retention policy exists to avoid.
   */
  it("shows a node that no root can reach", () => {
    const forest = toForest(
      graph(
        [node("island"), node("other")],
        [
          { from: "island", to: "other", label: null },
          { from: "other", to: "island", label: null },
        ],
      ),
    );
    expect(walk(forest)).toContain("island");
  });

  it("ignores an edge naming a node that is not in the graph", () => {
    const forest = toForest(
      graph([node("a")], [{ from: "a", to: "ghost", label: null }]),
    );
    expect(walk(forest)).toEqual(["a"]);
  });

  it("handles an empty graph", () => {
    expect(toForest(graph([]))).toEqual([]);
  });

  it("keeps the classes and groups the fold attached", () => {
    const forest = toForest(graph([node("a", { classes: ["root"], group: "caps" })]));
    expect(forest[0]?.node.classes).toEqual(["root"]);
    expect(forest[0]?.node.group).toBe("caps");
  });
});
