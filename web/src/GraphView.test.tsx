import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { GraphView } from "./GraphView";
import type { Graph } from "./graph";

const graph = (nodes: Graph["nodes"], edges: Graph["edges"] = []): Graph => ({
  direction: "LR",
  nodes,
  edges,
});

const node = (id: string, label: string, over: Partial<Graph["nodes"][0]> = {}) => ({
  id,
  label,
  classes: [],
  group: null,
  ...over,
});

describe("GraphView", () => {
  it("renders a node per graph node", () => {
    render(
      <GraphView
        graph={graph([node("a", "Alpha"), node("b", "Beta")])}
        empty="nothing"
      />,
    );
    expect(screen.getAllByTestId("graph-node")).toHaveLength(2);
  });

  it("shows the labels the fold produced, not the ids", () => {
    render(
      <GraphView graph={graph([node("cap1", "#1 Endpoint init [SEND]")])} empty="—" />,
    );
    expect(screen.getByText("#1 Endpoint init [SEND]")).toBeInTheDocument();
  });

  /** The edge label is what a derivation tree is *about* — which rights moved. */
  it("shows what the edge to a child meant", () => {
    render(
      <GraphView
        graph={graph(
          [node("a", "A"), node("b", "B")],
          [{ from: "a", to: "b", label: "RECV|MINT" }],
        )}
        empty="—"
      />,
    );
    expect(screen.getByText(/RECV\|MINT/)).toBeInTheDocument();
  });

  /**
   * A revisit must announce itself. Rendered as a plain node it would read as a
   * second grant of the same capability — the panel asserting something the guest
   * never did.
   */
  it("marks a revisited node rather than repeating it silently", () => {
    render(
      <GraphView
        graph={graph(
          [node("a", "A"), node("b", "B")],
          [
            { from: "a", to: "b", label: null },
            { from: "b", to: "a", label: null },
          ],
        )}
        empty="—"
      />,
    );
    expect(screen.getByText(/seen above/)).toBeInTheDocument();
  });

  /**
   * An empty graph is the normal state for the first second of a boot, and for a
   * guest that grants nothing. It must say so rather than render a blank box that
   * looks like a bug.
   */
  it("says why it is empty rather than showing nothing", () => {
    render(<GraphView graph={graph([])} empty="no capabilities granted yet" />);
    expect(screen.getByText("no capabilities granted yet")).toBeInTheDocument();
    expect(screen.queryAllByTestId("graph-node")).toHaveLength(0);
  });

  it("nests a child under its parent", () => {
    render(
      <GraphView
        graph={graph(
          [node("a", "A"), node("b", "B")],
          [{ from: "a", to: "b", label: null }],
        )}
        empty="—"
      />,
    );
    const rows = screen.getAllByTestId("graph-node");
    expect(rows[0]?.contains(rows[1] ?? null)).toBe(true);
  });
});
