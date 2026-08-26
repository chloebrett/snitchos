import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { Chart, compact } from "./Chart";

const series = (name: string, values: number[]) => ({
  name,
  points: values.map((v, i) => ({ t: i * 100, v })),
});

describe("Chart", () => {
  it("draws a path per series", () => {
    const { container } = render(
      <Chart series={[series("a", [1, 2, 3]), series("b", [3, 2, 1])]} />,
    );
    expect(container.querySelectorAll("path[d]")).toHaveLength(2);
  });

  /**
   * Identity is never colour alone — a reader who cannot distinguish two hues still
   * has the names, and the numbers stay in text tokens rather than the series colour.
   */
  it("names every series beside its swatch when there are several", () => {
    render(<Chart series={[series("heap", [1, 2]), series("sched", [2, 3])]} />);
    expect(screen.getByText("heap")).toBeInTheDocument();
    expect(screen.getByText("sched")).toBeInTheDocument();
  });

  /**
   * ...and no legend for a single series: the caller's title already names it, so
   * repeating it below states the same fact twice. Caught by a panel test finding the
   * name in two places at once.
   */
  it("does not repeat the name when there is only one series", () => {
    render(<Chart series={[series("snitchos.heap.bytes_used", [1, 2])]} />);
    expect(screen.queryByText("snitchos.heap.bytes_used")).not.toBeInTheDocument();
  });

  /**
   * Colour follows the entity, not its rank: two series must not be handed the same
   * slot, or a chart says two different things are one thing.
   */
  it("gives each series its own colour", () => {
    const { container } = render(
      <Chart series={[series("a", [1, 2]), series("b", [2, 3])]} />,
    );
    const strokes = [...container.querySelectorAll("path[d]")].map((p) =>
      p.getAttribute("stroke"),
    );
    expect(new Set(strokes).size).toBe(2);
  });

  /**
   * "No samples" is a normal state for the first second of a boot and must say so —
   * an empty axis frame reads as a broken chart.
   */
  it("says when there is nothing to plot", () => {
    render(<Chart series={[]} />);
    expect(screen.getByText(/no samples yet/)).toBeInTheDocument();
  });

  /** A gauge that has not moved is still a chart, not a divide-by-zero. */
  it("plots a constant series without producing a broken path", () => {
    const { container } = render(<Chart series={[series("flat", [5, 5, 5])]} />);
    const d = container.querySelector("path[d]")?.getAttribute("d") ?? "";
    expect(d).not.toMatch(/NaN|Infinity/);
    expect(d.length).toBeGreaterThan(0);
  });

  /** A single sample draws no line — but must not throw or emit a broken path. */
  it("survives a series with one sample", () => {
    const { container } = render(<Chart series={[series("one", [42])]} />);
    expect(container.querySelector("path[d]")?.getAttribute("d")).toBe("");
  });

  it("labels the axis with the unit it was given", () => {
    render(<Chart series={[series("a", [1, 2])]} unit="bytes" />);
    expect(screen.getByText("bytes")).toBeInTheDocument();
  });

  /** The chart is an image with a name, for a reader who is not looking at it. */
  it("describes itself to assistive technology", () => {
    render(<Chart series={[series("snitchos.sched.tasks_total", [1, 2])]} />);
    expect(screen.getByRole("img")).toHaveAccessibleName(
      /snitchos\.sched\.tasks_total over guest time/,
    );
  });
});

describe("compact", () => {
  /** Axis labels have to fit: `1.2M` where `1200000` would collide with its neighbour. */
  it("abbreviates large numbers", () => {
    expect(compact(1_200_000)).toBe("1.2M");
    expect(compact(4_500)).toBe("4.5k");
    expect(compact(2_000_000_000)).toBe("2.0G");
  });

  it("leaves small numbers alone", () => {
    expect(compact(42)).toBe("42");
    expect(compact(0)).toBe("0");
  });

  /** Guest counters do not go negative, but a derived rate can. */
  it("abbreviates negatives too", () => {
    expect(compact(-1_500)).toBe("-1.5k");
  });
});
