import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { type MetricSeries, TIMEBASE_HZ } from "./metrics";
import { MetricsPanel } from "./MetricsPanel";

const series = (name: string, over: Partial<MetricSeries> = {}): MetricSeries => ({
  name,
  kind: "Gauge",
  points: [
    [0, 1],
    [TIMEBASE_HZ, 2],
  ],
  ...over,
});

describe("MetricsPanel", () => {
  it("offers a button per group, with how many metrics it holds", () => {
    render(
      <MetricsPanel
        series={[series("snitchos.heap.a"), series("snitchos.heap.b"), series("snitchos.sched.c")]}
      />,
    );

    expect(screen.getByTestId("group-heap")).toHaveTextContent("2");
    expect(screen.getByTestId("group-sched")).toHaveTextContent("1");
  });

  /**
   * Small multiples: one chart per metric. A group mixes units — bytes beside block
   * counts — and a shared axis would be the dual-axis mistake.
   */
  it("draws a chart per metric in the selected group", () => {
    const { container } = render(
      <MetricsPanel series={[series("snitchos.heap.a"), series("snitchos.heap.b")]} />,
    );
    expect(container.querySelectorAll("figure")).toHaveLength(2);
  });

  it("switches group on click", async () => {
    render(<MetricsPanel series={[series("snitchos.heap.a"), series("snitchos.sched.b")]} />);

    await userEvent.click(screen.getByTestId("group-sched"));
    expect(screen.getByTestId("group-sched")).toHaveAttribute("aria-pressed", "true");
    expect(screen.getByText("b")).toBeInTheDocument();
  });

  /** The prefix the group already states is dead weight in a small chart title. */
  it("titles a chart without the prefix its group carries", () => {
    render(<MetricsPanel series={[series("snitchos.heap.bytes_used")]} />);
    expect(screen.getByText("bytes_used")).toBeInTheDocument();
  });

  /**
   * A counter's axis has to say the rate is *derived*. The guest never emitted these
   * numbers, and a chart that implies otherwise is misreporting its own provenance.
   */
  it("marks a counter's axis as derived", () => {
    render(<MetricsPanel series={[series("snitchos.sched.switches", { kind: "Counter" })]} />);
    expect(screen.getByText(/derived/)).toBeInTheDocument();
  });

  it("does not call a gauge derived", () => {
    render(<MetricsPanel series={[series("snitchos.heap.used", { kind: "Gauge" })]} />);
    expect(screen.queryByText(/derived/)).not.toBeInTheDocument();
  });

  /**
   * Histograms are left out rather than drawn as a line of something else. If a group
   * holds nothing but histograms it should not appear at all — an empty group button
   * would promise a view that cannot exist.
   */
  it("omits a group that holds only histograms", () => {
    render(
      <MetricsPanel
        series={[
          series("snitchos.lat.buckets", { kind: "Histogram" }),
          series("snitchos.heap.a"),
        ]}
      />,
    );

    expect(screen.queryByTestId("group-lat")).not.toBeInTheDocument();
    expect(screen.getByTestId("group-heap")).toBeInTheDocument();
  });

  /** Before any metric arrives, say so rather than render an empty frame. */
  it("says when there are no metrics", () => {
    render(<MetricsPanel series={[]} />);
    expect(screen.getByText(/no metrics yet/)).toBeInTheDocument();
  });
});
