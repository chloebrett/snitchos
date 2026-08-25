import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import type { FrameView } from "./frames";
import { TelemetryPane } from "./TelemetryPane";

const frame = (over: Partial<FrameView> = {}): FrameView => ({
  kind: "SpanStart",
  name: null,
  t: null,
  value: null,
  ...over,
});

describe("TelemetryPane", () => {
  it("renders a row per frame", () => {
    render(<TelemetryPane frames={[frame(), frame({ kind: "Metric" })]} />);
    expect(screen.getAllByTestId("frame-row")).toHaveLength(2);
  });

  it("shows the resolved name beside the kind", () => {
    render(<TelemetryPane frames={[frame({ name: "kernel.boot" })]} />);
    expect(screen.getByText("kernel.boot")).toBeInTheDocument();
    expect(screen.getByText("SpanStart")).toBeInTheDocument();
  });

  /**
   * `null` means the wire cited a `StringId` nothing has registered yet. The Rust
   * side goes out of its way to keep that distinct from a name, so the UI must not
   * spend it by printing "null" or inventing a placeholder.
   */
  it("renders an unresolved name as blank, never as a stand-in", () => {
    render(<TelemetryPane frames={[frame({ name: null })]} />);
    const row = screen.getByTestId("frame-row");
    expect(row.textContent).toBe("SpanStart");
  });

  it("shows a metric's value", () => {
    render(<TelemetryPane frames={[frame({ kind: "Metric", value: 1234 })]} />);
    expect(screen.getByText("1234")).toBeInTheDocument();
  });

  /** Zero is a value, and a falsy-check would swallow it. */
  it("shows a zero value rather than hiding it", () => {
    render(<TelemetryPane frames={[frame({ kind: "Metric", value: 0 })]} />);
    expect(screen.getByText("0")).toBeInTheDocument();
  });

  it("counts frames, and pluralises honestly", () => {
    const { rerender } = render(<TelemetryPane frames={[frame()]} />);
    expect(screen.getByText(/1 frame$/)).toBeInTheDocument();

    rerender(<TelemetryPane frames={[frame(), frame()]} />);
    expect(screen.getByText(/2 frames$/)).toBeInTheDocument();
  });

  it("renders an empty stream without rows", () => {
    render(<TelemetryPane frames={[]} />);
    expect(screen.queryAllByTestId("frame-row")).toHaveLength(0);
  });
});
