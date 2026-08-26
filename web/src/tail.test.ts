import { describe, expect, it } from "vitest";
import type { FrameView } from "./frames";
import { appendCollapsed, type FrameRow, kindsIn, visibleRows } from "./tail";

const view = (kind: string, over: Partial<FrameView> = {}): FrameView => ({
  kind,
  name: null,
  t: null,
  value: null,
  ...over,
});

const row = (kind: string, count = 1): FrameRow => ({ view: view(kind), count });

describe("appendCollapsed", () => {
  /**
   * The reason this exists. A guest emitting thousands of switches a second fills any
   * reasonable window with them; as one counted row it costs a line.
   */
  it("collapses a run of like frames into one counted row", () => {
    const switches = Array.from({ length: 500 }, () => view("ContextSwitch"));
    const rows = appendCollapsed([], switches, 400);

    expect(rows).toHaveLength(1);
    expect(rows[0]?.count).toBe(500);
  });

  it("keeps unlike frames apart", () => {
    const rows = appendCollapsed([], [view("SpanStart"), view("SpanEnd")], 400);
    expect(rows.map((r) => r.view.kind)).toEqual(["SpanStart", "SpanEnd"]);
  });

  /**
   * Kind alone would fold every metric into one row and throw away the names, which
   * are the entire content of a metric frame.
   */
  it("does not collapse metrics that differ only by name", () => {
    const rows = appendCollapsed(
      [],
      [
        view("Metric", { name: "snitchos.heap.bytes_used", value: 1 }),
        view("Metric", { name: "snitchos.sched.tasks_total", value: 2 }),
      ],
      400,
    );
    expect(rows).toHaveLength(2);
  });

  /**
   * A collapsed run shows the *latest* sample, so a counted metric row is a live
   * reading rather than a stale first observation.
   */
  it("shows the most recent frame of a run", () => {
    const rows = appendCollapsed(
      [],
      [
        view("Metric", { name: "count", value: 10 }),
        view("Metric", { name: "count", value: 11 }),
        view("Metric", { name: "count", value: 12 }),
      ],
      400,
    );

    expect(rows).toHaveLength(1);
    expect(rows[0]?.count).toBe(3);
    expect(rows[0]?.view.value).toBe(12);
  });

  /** Only *consecutive* frames collapse — an interruption starts a new run. */
  it("starts a new row when a run is interrupted", () => {
    const rows = appendCollapsed(
      [],
      [view("ContextSwitch"), view("SpanStart"), view("ContextSwitch")],
      400,
    );
    expect(rows.map((r) => r.view.kind)).toEqual([
      "ContextSwitch",
      "SpanStart",
      "ContextSwitch",
    ]);
  });

  it("continues a run that began in an earlier batch", () => {
    const first = appendCollapsed([], [view("ContextSwitch")], 400);
    const second = appendCollapsed(first, [view("ContextSwitch")], 400);

    expect(second).toHaveLength(1);
    expect(second[0]?.count).toBe(2);
  });

  /**
   * The cap counts **rows**, not frames — capping frames is what limited the window
   * to a few hundred milliseconds of switch traffic in the first place.
   */
  it("caps rows, so a collapsed run costs one of them", () => {
    const rows = appendCollapsed([], [view("a"), view("b"), view("c")], 2);
    expect(rows.map((r) => r.view.kind)).toEqual(["b", "c"]);
  });

  it("drops the oldest rows when over the cap", () => {
    const rows = appendCollapsed([row("old"), row("mid")], [view("new")], 2);
    expect(rows.map((r) => r.view.kind)).toEqual(["mid", "new"]);
  });

  /** Identity when nothing arrives, so React can skip a re-render. */
  it("returns the same array when there is nothing new", () => {
    const rows = [row("a")];
    expect(appendCollapsed(rows, [], 10)).toBe(rows);
  });

  /** The input array is React state and must not be mutated. */
  it("does not mutate the rows it was given", () => {
    const rows = [row("ContextSwitch")];
    appendCollapsed(rows, [view("ContextSwitch")], 10);
    expect(rows[0]?.count).toBe(1);
  });
});

describe("kindsIn", () => {
  it("lists the distinct kinds in first-seen order", () => {
    expect(kindsIn([row("b"), row("a"), row("b")])).toEqual(["b", "a"]);
  });

  it("is empty for an empty tail", () => {
    expect(kindsIn([])).toEqual([]);
  });
});

describe("visibleRows", () => {
  it("hides the kinds it is told to", () => {
    const rows = [row("ContextSwitch"), row("SpanStart")];
    expect(visibleRows(rows, new Set(["ContextSwitch"]))).toHaveLength(1);
  });

  it("shows everything when nothing is hidden", () => {
    const rows = [row("a"), row("b")];
    expect(visibleRows(rows, new Set())).toHaveLength(2);
  });

  /** Hiding one kind must not disturb the order of what remains. */
  it("preserves the order of what is left", () => {
    const rows = [row("a"), row("noise"), row("b")];
    expect(visibleRows(rows, new Set(["noise"])).map((r) => r.view.kind)).toEqual([
      "a",
      "b",
    ]);
  });
});
