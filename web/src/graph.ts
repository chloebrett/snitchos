/**
 * The shape `diagram::model::Graph::to_json` emits, and turning it into something a
 * component can render.
 *
 * The folds that produce these graphs are the same ones that generate the committed
 * `docs/generated/*.md` — one implementation, three renderers (mermaid, DOT, and this).
 */

export interface GraphNode {
  id: string;
  label: string;
  /** Style classes the fold attached, e.g. `root`. */
  classes: string[];
  /** A cluster the fold grouped this node into, or `null`. */
  group: string | null;
}

export interface GraphEdge {
  from: string;
  to: string;
  /** What the edge means — the rights transferred, say. `null` when unlabelled. */
  label: string | null;
}

export interface Graph {
  direction: "LR" | "TD";
  nodes: GraphNode[];
  edges: GraphEdge[];
}

/** A node placed in a tree, with the edge that led to it. */
export interface TreeNode {
  node: GraphNode;
  /** The label of the edge from its parent, if any. */
  via: string | null;
  children: TreeNode[];
  /**
   * True when this node has already appeared elsewhere in the tree, so it is shown
   * here as a reference rather than expanded again.
   *
   * A derivation tree is usually a tree, but nothing in the wire *guarantees* it: a
   * capability can be reached by more than one path, and the switch-transition graph
   * is frankly cyclic (task A yields to B yields back to A). Marking a revisit rather
   * than descending is what keeps rendering finite.
   */
  revisited: boolean;
}

/**
 * Arrange a graph as a forest for display.
 *
 * Roots are nodes nothing points at. Anything left over after walking from those —
 * a cycle with no entry point — becomes a root too, in declaration order, because a
 * node that exists must be visible somewhere: silently dropping it would make the
 * panel disagree with the graph it was given.
 *
 * **Terminating on cyclic input is the whole reason this is a function with tests.**
 * The switch-transition fold produces cycles as a matter of course, and a naive
 * descent would recurse until the tab died.
 */
export function toForest(graph: Graph): TreeNode[] {
  const byId = new Map(graph.nodes.map((n) => [n.id, n]));
  const childrenOf = new Map<string, GraphEdge[]>();
  const hasParent = new Set<string>();

  for (const edge of graph.edges) {
    if (!byId.has(edge.from) || !byId.has(edge.to)) continue; // an edge to nowhere
    const list = childrenOf.get(edge.from) ?? [];
    list.push(edge);
    childrenOf.set(edge.from, list);
    hasParent.add(edge.to);
  }

  const placed = new Set<string>();

  const build = (node: GraphNode, via: string | null): TreeNode => {
    if (placed.has(node.id)) {
      return { node, via, children: [], revisited: true };
    }
    placed.add(node.id);
    const children = (childrenOf.get(node.id) ?? []).flatMap((edge) => {
      const child = byId.get(edge.to);
      return child ? [build(child, edge.label)] : [];
    });
    return { node, via, children, revisited: false };
  };

  const forest = graph.nodes
    .filter((n) => !hasParent.has(n.id))
    .map((n) => build(n, null));

  // Whatever the roots could not reach — a cycle with no way in.
  for (const node of graph.nodes) {
    if (!placed.has(node.id)) forest.push(build(node, null));
  }
  return forest;
}
