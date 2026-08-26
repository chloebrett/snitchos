import { describe, expect, it } from "vitest";
import {
  chartable,
  groupOf,
  groupsOf,
  type MetricSeries,
  pointsFor,
  rateOf,
  shortName,
  TIMEBASE_HZ,
  unitFor,
} from "./metrics";

const series = (over: Partial<MetricSeries> = {}): MetricSeries => ({
  name: "snitchos.heap.bytes_used",
  kind: "Gauge",
  points: [],
  ...over,
});

describe("rateOf", () => {
  it("reports change per second of guest time", () => {
    // One guest second apart, ten more each time.
    const points: Array<[number, number]> = [
      [0, 0],
      [TIMEBASE_HZ, 10],
      [TIMEBASE_HZ * 2, 20],
    ];
    expect(rateOf(points).map((p) => p.v)).toEqual([10, 10]);
  });

  /**
   * Two samples can share a guest tick, and dividing by that gap is `Infinity` on an
   * axis.
   */
  it("skips a pair that shares a timestamp rather than dividing by zero", () => {
    const rate = rateOf([
      [100, 1],
      [100, 2],
    ]);
    expect(rate).toEqual([]);
  });

  /**
   * A counter reset — a restarted guest, a re-registered counter — makes the delta
   * negative. Plotted raw it is an enormous downward spike that reads as a real
   * event; every time-series system treats it as a reset instead.
   */
  it("treats a counter going backwards as a reset, not a negative rate", () => {
    const rate = rateOf([
      [0, 1000],
      [TIMEBASE_HZ, 5],
    ]);
    expect(rate.map((p) => p.v)).toEqual([0]);
  });

  it("yields nothing for fewer than two samples", () => {
    expect(rateOf([])).toEqual([]);
    expect(rateOf([[0, 1]])).toEqual([]);
  });

  /** The rate is attributed to the *later* sample — the interval it describes ends there. */
  it("stamps each rate at the end of the interval it covers", () => {
    const rate = rateOf([
      [0, 0],
      [TIMEBASE_HZ, 1],
    ]);
    expect(rate[0]?.t).toBe(TIMEBASE_HZ);
  });
});

describe("pointsFor", () => {
  /** A counter's raw value only ever climbs; the rate is the quantity worth seeing. */
  it("charts a counter as its rate", () => {
    const s = series({
      kind: "Counter",
      points: [
        [0, 0],
        [TIMEBASE_HZ, 7],
      ],
    });
    expect(pointsFor(s)).toEqual([{ t: TIMEBASE_HZ, v: 7 }]);
  });

  it("charts a gauge as its value", () => {
    const s = series({
      kind: "Gauge",
      points: [
        [0, 5],
        [1, 6],
      ],
    });
    expect(pointsFor(s)).toEqual([
      { t: 0, v: 5 },
      { t: 1, v: 6 },
    ]);
  });

  /**
   * An undescribed metric is plotted as-is rather than guessed at: assuming it is a
   * counter would silently show a rate for something that may be a gauge.
   */
  it("charts an undescribed metric as its raw value", () => {
    const s = series({ kind: null, points: [[0, 5]] });
    expect(pointsFor(s)).toEqual([{ t: 0, v: 5 }]);
  });
});

describe("unitFor", () => {
  /**
   * A rate is *derived* — the guest never emitted it. Saying so on the axis is the
   * difference between a computed view and a claim about what was measured.
   */
  it("marks a counter's axis as derived", () => {
    expect(unitFor(series({ kind: "Counter" }))).toContain("derived");
  });

  it("does not call a gauge derived", () => {
    expect(unitFor(series({ kind: "Gauge" }))).not.toContain("derived");
  });
});

describe("chartable", () => {
  /**
   * A line through a histogram plots *something* — a sum, a count — under the
   * histogram's name, which misrepresents what the guest measured. Absent beats
   * misleading; they can be rendered properly later.
   */
  it("excludes histograms", () => {
    expect(chartable(series({ kind: "Histogram" }))).toBe(false);
  });

  it("includes counters, gauges and undescribed metrics", () => {
    expect(chartable(series({ kind: "Counter" }))).toBe(true);
    expect(chartable(series({ kind: "Gauge" }))).toBe(true);
    expect(chartable(series({ kind: null }))).toBe(true);
  });
});

describe("groupOf", () => {
  it("takes the segment after the snitchos prefix", () => {
    expect(groupOf("snitchos.heap.bytes_used")).toBe("heap");
    expect(groupOf("snitchos.sched.context_switches_total")).toBe("sched");
  });

  /** Per-task metrics carry a name in the middle and still belong to `task`. */
  it("groups per-task metrics together", () => {
    expect(groupOf("snitchos.task.stitch_repl.cpu_time_ticks")).toBe("task");
  });

  it("falls back to the whole first segment for an unprefixed name", () => {
    expect(groupOf("custom_metric")).toBe("custom_metric");
  });
});

describe("groupsOf", () => {
  it("collects metrics under their group, in first-seen order", () => {
    const groups = groupsOf([
      series({ name: "snitchos.sched.a" }),
      series({ name: "snitchos.heap.b" }),
      series({ name: "snitchos.sched.c" }),
    ]);

    expect(groups.map((g) => g.name)).toEqual(["sched", "heap"]);
    expect(groups[0]?.series).toHaveLength(2);
  });

  it("leaves histograms out of every group", () => {
    const groups = groupsOf([
      series({ name: "snitchos.heap.a", kind: "Histogram" }),
      series({ name: "snitchos.heap.b", kind: "Gauge" }),
    ]);

    expect(groups[0]?.series.map((s) => s.name)).toEqual(["snitchos.heap.b"]);
  });

  it("is empty for no metrics", () => {
    expect(groupsOf([])).toEqual([]);
  });
});

describe("shortName", () => {
  /** A chart in a small box has no room for the prefix its group already states. */
  it("drops the prefix its group already carries", () => {
    expect(shortName("snitchos.heap.bytes_used")).toBe("bytes_used");
    expect(shortName("snitchos.task.idle.runs_total")).toBe("idle.runs_total");
  });

  it("leaves an unprefixed name alone", () => {
    expect(shortName("custom")).toBe("custom");
  });
});
