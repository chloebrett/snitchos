/**
 * Turning samples into geometry.
 *
 * Separated from the chart component because every way a chart goes wrong at the
 * edges is arithmetic, not rendering: a series with one point, a gauge that has not
 * moved, an empty series during the first second of a boot. Each of those divides by
 * a zero-width domain if nobody thought about it, and the symptom is a blank panel or
 * an `NaN` in a path attribute rather than an error.
 */

/** A sample: guest time, and the value at it. */
export interface Point {
  t: number;
  v: number;
}

/** An inclusive numeric range. */
export interface Extent {
  min: number;
  max: number;
}

/** The plot area, in pixels. */
export interface Box {
  width: number;
  height: number;
}

/**
 * The range a set of numbers covers, or `null` if there are none.
 *
 * `null` rather than `{min: 0, max: 0}`: "no data" and "all zeroes" are different
 * states, and a chart should say *waiting* for one and draw a flat line for the
 * other.
 */
export function extentOf(values: readonly number[]): Extent | null {
  if (values.length === 0) return null;
  let min = values[0] as number;
  let max = min;
  for (const v of values) {
    if (v < min) min = v;
    if (v > max) max = v;
  }
  return { min, max };
}

/**
 * Widen a zero-width extent so it can be divided by.
 *
 * A constant series — a gauge that has not moved, or a single sample — has
 * `min === max`, and every scale over it divides by zero. Padding by a tenth of the
 * value (or by 1 at zero) puts the line in the middle of the plot, which is also
 * what a reader expects: a flat line at mid-height reads as "steady", where a flat
 * line pinned to the axis reads as "at the minimum".
 */
export function padded(extent: Extent): Extent {
  if (extent.min !== extent.max) return extent;
  const pad = extent.min === 0 ? 1 : Math.abs(extent.min) * 0.1;
  return { min: extent.min - pad, max: extent.max + pad };
}

/**
 * A linear map from `domain` onto `[0, size]`.
 *
 * Total: a zero-width domain maps everything to the midpoint rather than producing
 * `Infinity` or `NaN`, so a caller that forgot {@link padded} gets a flat line rather
 * than an unparseable path.
 */
export function scale(domain: Extent, size: number): (value: number) => number {
  const span = domain.max - domain.min;
  if (span === 0) return () => size / 2;
  return (value) => ((value - domain.min) / span) * size;
}

/**
 * An SVG path through `points`, with time along x and value up y.
 *
 * Empty for fewer than two points: a single sample is a dot, not a line, and a path
 * with one command draws nothing while still being valid — which is worse, because it
 * looks like a rendering bug rather than a lack of data.
 */
export function linePath(points: readonly Point[], box: Box): string {
  if (points.length < 2) return "";

  const x = scale(
    padded(extentOf(points.map((p) => p.t)) ?? { min: 0, max: 1 }),
    box.width,
  );
  const y = scale(
    padded(extentOf(points.map((p) => p.v)) ?? { min: 0, max: 1 }),
    box.height,
  );

  return points
    .map((p, i) => {
      // SVG y grows downward; values grow upward.
      const px = x(p.t).toFixed(2);
      const py = (box.height - y(p.v)).toFixed(2);
      return `${i === 0 ? "M" : "L"}${px},${py}`;
    })
    .join(" ");
}

/**
 * Tick values across `extent`, at a step a person would have chosen: 1, 2 or 5 times
 * a power of ten.
 *
 * `count` is a target, not a promise — snapping the step to a readable number is what
 * matters, and forcing an exact count is what produces axes labelled 3.7, 7.4, 11.1.
 */
export function ticks(extent: Extent, count = 4): number[] {
  const { min, max } = padded(extent);
  const rough = (max - min) / Math.max(1, count);
  const magnitude = 10 ** Math.floor(Math.log10(rough));
  const step =
    [1, 2, 5, 10].map((m) => m * magnitude).find((s) => s >= rough) ?? magnitude * 10;

  const out: number[] = [];
  for (let v = Math.ceil(min / step) * step; v <= max; v += step) {
    // Floating-point steps accumulate: 0.1 + 0.2 is famously not 0.3, and an axis
    // labelled 0.30000000000000004 is the visible form of that.
    out.push(Number.parseFloat(v.toPrecision(12)));
  }
  return out;
}
