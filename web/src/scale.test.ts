import { describe, expect, it } from "vitest";
import { type Box, extentOf, linePath, type Point, padded, scale, ticks } from "./scale";

const box: Box = { width: 100, height: 50 };
const pt = (t: number, v: number): Point => ({ t, v });

describe("extentOf", () => {
  it("spans the values", () => {
    expect(extentOf([3, 1, 2])).toEqual({ min: 1, max: 3 });
  });

  /**
   * `null`, not `{min: 0, max: 0}`. "No data" and "all zeroes" are different states:
   * one should say *waiting*, the other should draw a flat line.
   */
  it("is null when there are no values", () => {
    expect(extentOf([])).toBeNull();
  });

  it("handles negatives", () => {
    expect(extentOf([-5, 5])).toEqual({ min: -5, max: 5 });
  });
});

describe("padded", () => {
  it("leaves a real range alone", () => {
    expect(padded({ min: 1, max: 3 })).toEqual({ min: 1, max: 3 });
  });

  /**
   * A gauge that has not moved has `min === max`, and every scale over it divides by
   * zero. Widening puts the flat line mid-plot, which reads as "steady" rather than
   * "at the minimum".
   */
  it("widens a constant series so it can be divided by", () => {
    const p = padded({ min: 100, max: 100 });
    expect(p.max).toBeGreaterThan(p.min);
  });

  /** Ten percent of zero is zero, so zero needs its own case. */
  it("widens a constant zero series", () => {
    expect(padded({ min: 0, max: 0 })).toEqual({ min: -1, max: 1 });
  });
});

describe("scale", () => {
  it("maps the domain onto the range", () => {
    const s = scale({ min: 0, max: 10 }, 100);
    expect(s(0)).toBe(0);
    expect(s(5)).toBe(50);
    expect(s(10)).toBe(100);
  });

  /**
   * Total rather than correct-input-only: a caller that forgot to pad gets a flat
   * line rather than `NaN` in a path attribute, which renders as nothing and looks
   * like a rendering bug.
   */
  it("maps a zero-width domain to the midpoint rather than dividing by zero", () => {
    const s = scale({ min: 7, max: 7 }, 100);
    expect(s(7)).toBe(50);
    expect(Number.isFinite(s(7))).toBe(true);
  });
});

describe("linePath", () => {
  it("draws through the points, moving first and lining after", () => {
    const d = linePath([pt(0, 0), pt(1, 1)], box);
    expect(d.startsWith("M")).toBe(true);
    expect(d).toContain("L");
  });

  /** SVG y grows downward; a rising value must go *up* the plot. */
  it("puts a higher value higher on screen", () => {
    const d = linePath([pt(0, 0), pt(1, 10)], box);
    const [first, second] = d.split(" ");
    const y = (cmd: string) => Number.parseFloat(cmd.split(",")[1] as string);
    expect(y(second as string)).toBeLessThan(y(first as string));
  });

  /**
   * One sample is a dot, not a line. A one-command path is valid and draws nothing,
   * which looks like a bug rather than like a lack of data.
   */
  it("is empty for fewer than two points", () => {
    expect(linePath([pt(0, 1)], box)).toBe("");
    expect(linePath([], box)).toBe("");
  });

  /** The case that divides by zero if nobody widened the domain. */
  it("draws a constant series as a finite path", () => {
    const d = linePath([pt(0, 5), pt(1, 5), pt(2, 5)], box);
    expect(d).not.toContain("NaN");
    expect(d).not.toContain("Infinity");
  });

  it("never emits a non-finite coordinate", () => {
    const d = linePath([pt(0, 0), pt(0, 0), pt(0, 0)], box);
    expect(d).not.toMatch(/NaN|Infinity/);
  });
});

describe("ticks", () => {
  /** Steps a person would have chosen: 1, 2 or 5 times a power of ten. */
  it("uses a readable step", () => {
    const t = ticks({ min: 0, max: 100 }, 4);
    const step = (t[1] as number) - (t[0] as number);
    expect([1, 2, 5, 10, 20, 25, 50].includes(step)).toBe(true);
  });

  it("covers the extent", () => {
    const t = ticks({ min: 0, max: 10 }, 4);
    expect(t[0]).toBeLessThanOrEqual(0);
    expect(t[t.length - 1]).toBeGreaterThanOrEqual(9);
  });

  /**
   * Floating-point steps accumulate, and an axis labelled `0.30000000000000004` is
   * the visible form of that.
   */
  it("does not produce floating-point noise in its labels", () => {
    for (const v of ticks({ min: 0, max: 1 }, 5)) {
      expect(String(v).length).toBeLessThan(8);
    }
  });

  /** A constant series still needs an axis, so this must terminate and produce one. */
  it("produces ticks for a constant extent", () => {
    const t = ticks({ min: 5, max: 5 }, 4);
    expect(t.length).toBeGreaterThan(0);
  });
});
