import { type Graph, type TreeNode, toForest } from "./graph";

/**
 * A folded graph, as an indented tree.
 *
 * Indentation rather than a drawn graph, deliberately. The panels this serves are
 * *derivation* and *call* structures — who granted what to whom, which span opened
 * inside which — and those are read, not admired: you want to find a name, follow a
 * line, and see the rights on the edge that got you there. An indented tree gives
 * that for free, in text you can select, with no layout engine and nothing to
 * re-solve when a frame arrives.
 *
 * The picture already exists anyway: the same fold renders to mermaid in
 * `docs/generated/`. This is the view that picture cannot be.
 */
export function GraphView({ graph, empty }: { graph: Graph; empty: string }) {
  const forest = toForest(graph);

  if (forest.length === 0) {
    return <p className="px-3 py-2 text-neutral-600 text-xs italic">{empty}</p>;
  }

  return (
    <ul className="px-2 py-1 font-mono text-[0.72rem]" data-testid="graph">
      {forest.map((tree) => (
        <TreeRow key={tree.node.id} tree={tree} depth={0} />
      ))}
    </ul>
  );
}

function TreeRow({ tree, depth }: { tree: TreeNode; depth: number }) {
  const isRoot = tree.node.classes.includes("root");

  return (
    <li data-testid="graph-node" data-revisited={tree.revisited || undefined}>
      <div
        className="flex items-baseline gap-1.5 whitespace-nowrap py-px"
        style={{ paddingLeft: `${depth * 0.9}rem` }}
      >
        {tree.via !== null && <span className="text-neutral-600">─{tree.via}→</span>}
        <span
          className={
            tree.revisited
              ? "text-neutral-600 italic"
              : isRoot
                ? "font-semibold text-emerald-400"
                : "text-sky-300"
          }
          title={tree.node.group ?? undefined}
        >
          {tree.node.label}
        </span>
        {/* A revisit is a reference to a node expanded elsewhere. Saying so is what
            stops the reader believing the same capability was granted twice. */}
        {tree.revisited && <span className="text-neutral-700">↩ seen above</span>}
      </div>
      {tree.children.length > 0 && (
        <ul>
          {tree.children.map((child) => (
            <TreeRow
              key={`${tree.node.id}/${child.node.id}`}
              tree={child}
              depth={depth + 1}
            />
          ))}
        </ul>
      )}
    </li>
  );
}
