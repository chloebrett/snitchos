import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import type { FrameView } from "./frames";
import { TelemetryPane } from "./TelemetryPane";
import type { FrameRow } from "./tail";

const frame = (over: Partial<FrameView> = {}): FrameView => ({
  kind: "SpanStart",
  name: null,
  t: null,
  value: null,
  ...over,
});

const rowOf = (over: Partial<FrameView> = {}, count = 1): FrameRow => ({
  view: frame(over),
  count,
});

describe("TelemetryPane", () => {
  it("renders a row per frame", () => {
    render(<TelemetryPane rows={[rowOf(), rowOf({ kind: "Metric" })]} />);
    expect(screen.getAllByTestId("frame-row")).toHaveLength(2);
  });

  it("shows the resolved name beside the kind", () => {
    render(<TelemetryPane rows={[rowOf({ name: "kernel.boot" })]} />);
    // Scoped to the row: the kind also appears as a filter chip, so an unscoped
    // query matches twice.
    const row = within(screen.getByTestId("frame-row"));
    expect(row.getByText("kernel.boot")).toBeInTheDocument();
    expect(row.getByText("SpanStart")).toBeInTheDocument();
  });

  /**
   * `null` means the wire cited a `StringId` nothing has registered yet. The Rust
   * side goes out of its way to keep that distinct from a name, so the UI must not
   * spend it by printing "null" or inventing a placeholder.
   */
  it("renders an unresolved name as blank, never as a stand-in", () => {
    render(<TelemetryPane rows={[rowOf({ name: null })]} />);
    const row = screen.getByTestId("frame-row");
    expect(row.textContent).toBe("SpanStart");
  });

  it("shows a metric's value", () => {
    render(<TelemetryPane rows={[rowOf({ kind: "Metric", value: 1234 })]} />);
    expect(screen.getByText("1234")).toBeInTheDocument();
  });

  /** Zero is a value, and a falsy-check would swallow it. */
  it("shows a zero value rather than hiding it", () => {
    render(<TelemetryPane rows={[rowOf({ kind: "Metric", value: 0 })]} />);
    expect(screen.getByText("0")).toBeInTheDocument();
  });

  /**
   * A collapsed run says how many frames it stands for. Without the count the row
   * would claim one switch happened where five hundred did.
   */
  it("shows how many frames a collapsed run holds", () => {
    render(<TelemetryPane rows={[rowOf({ kind: "ContextSwitch" }, 500)]} />);
    expect(screen.getByText("×500")).toBeInTheDocument();
  });

  /** A single frame is not annotated — "×1" is noise. */
  it("does not annotate an uncollapsed frame", () => {
    render(<TelemetryPane rows={[rowOf({ kind: "ContextSwitch" })]} />);
    expect(screen.queryByText("×1")).not.toBeInTheDocument();
  });

  /**
   * The DevTools move: one click silences the noisiest kind. Complements collapsing
   * rather than replacing it — collapsing summarises, hiding focuses.
   */
  it("hides a kind when its chip is clicked, and restores it", async () => {
    render(
      <TelemetryPane
        rows={[rowOf({ kind: "ContextSwitch" }, 500), rowOf({ kind: "SpanStart" })]}
      />,
    );
    expect(screen.getAllByTestId("frame-row")).toHaveLength(2);

    await userEvent.click(screen.getByTestId("filter-ContextSwitch"));
    expect(screen.getAllByTestId("frame-row")).toHaveLength(1);
    expect(screen.getByTestId("filter-ContextSwitch")).toHaveAttribute(
      "aria-pressed",
      "false",
    );

    await userEvent.click(screen.getByTestId("filter-ContextSwitch"));
    expect(screen.getAllByTestId("frame-row")).toHaveLength(2);
  });

  /** A chip per kind present, so the filter is discoverable without documentation. */
  it("offers a chip for each kind in the tail", () => {
    render(<TelemetryPane rows={[rowOf({ kind: "A" }), rowOf({ kind: "B" })]} />);
    expect(screen.getByTestId("filter-A")).toBeInTheDocument();
    expect(screen.getByTestId("filter-B")).toBeInTheDocument();
  });

  it("renders an empty stream without rows", () => {
    render(<TelemetryPane rows={[]} />);
    expect(screen.queryAllByTestId("frame-row")).toHaveLength(0);
  });
});
