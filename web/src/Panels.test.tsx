import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import type { Views } from "./frames";
import type { Graph } from "./graph";
import { Panels } from "./Panels";

const empty: Graph = { direction: "TD", nodes: [], edges: [] };

const oneNode = (label: string): Graph => ({
  direction: "TD",
  nodes: [{ id: "n", label, classes: [], group: null }],
  edges: [],
});

const views = (over: Partial<Views> = {}): Views => ({
  caps: oneNode("a capability"),
  spans: oneNode("a span"),
  switches: oneNode("a switch"),
  metrics: [],
  durableFrames: 12,
  ...over,
});

describe("Panels", () => {
  it("opens on the capability tree", () => {
    render(<Panels views={views()} frames={[]} />);
    expect(screen.getByText("a capability")).toBeInTheDocument();
  });

  it("switches between views", async () => {
    render(<Panels views={views()} frames={[]} />);

    await userEvent.click(screen.getByTestId("tab-spans"));
    expect(screen.getByText("a span")).toBeInTheDocument();

    await userEvent.click(screen.getByTestId("tab-switches"));
    expect(screen.getByText("a switch")).toBeInTheDocument();
  });

  /**
   * "No source yet" and "a source that has produced nothing" are different states,
   * and conflating them would show an empty capability tree during boot — reading as
   * "this guest granted nothing" when the truth is "nothing has been asked yet".
   */
  it("distinguishes having no views from having empty ones", () => {
    const { rerender } = render(<Panels views={null} frames={[]} />);
    expect(screen.getByText(/waiting for the guest/)).toBeInTheDocument();

    rerender(<Panels views={views({ caps: empty })} frames={[]} />);
    expect(screen.getByText(/no capabilities derived yet/)).toBeInTheDocument();
  });

  /**
   * The durable bucket has no ceiling by design, so it is shown. If it climbs without
   * limit on a long run, the assumption behind that design was wrong — and the only
   * way to notice is to be able to see it.
   */
  it("shows how many cumulative frames are being kept", () => {
    render(<Panels views={views({ durableFrames: 41 })} frames={[]} />);
    expect(screen.getByTestId("durable-count")).toHaveTextContent("41");
  });

  it("still offers the raw frame tail", async () => {
    render(
      <Panels
        views={views()}
        frames={[
          {
            view: { kind: "SpanStart", name: "kernel.boot", t: 1, value: null },
            count: 1,
          },
        ]}
      />,
    );

    await userEvent.click(screen.getByTestId("tab-frames"));
    expect(screen.getByText("kernel.boot")).toBeInTheDocument();
  });

  it("marks the active tab for assistive technology", async () => {
    render(<Panels views={views()} frames={[]} />);
    expect(screen.getByTestId("tab-caps")).toHaveAttribute("aria-pressed", "true");

    await userEvent.click(screen.getByTestId("tab-spans"));
    expect(screen.getByTestId("tab-spans")).toHaveAttribute("aria-pressed", "true");
    expect(screen.getByTestId("tab-caps")).toHaveAttribute("aria-pressed", "false");
  });
});
