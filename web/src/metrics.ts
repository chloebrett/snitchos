/**
 * Deciding what to plot, and how.
 *
 * The guest emits ~60 metrics every heartbeat. Sixty charts is not a dashboard, and
 * one chart with sixty series is worse — so the work here is grouping, and deciding
 * what a given metric's *interesting quantity* actually is.
 */

import type { Point } from "./scale";

/** How the guest describes a metric, mirroring `protocol::MetricKind`. */
export type MetricKind = "Counter" | "Gauge" | "Histogram";

/** One metric's history, as it crosses from Rust. */
export interface MetricSeries {
  name: string;
  /** `null` when the guest has not described it yet. */
  kind: MetricKind | null;
  /** `[guestTime, value]` pairs. */
  points: Array<[number, number]>;
}

/** Guest timer ticks per second — the same timebase the pacer runs against. */
export const TIMEBASE_HZ = 10_000_000;

/**
 * A counter's rate, per second of guest time.
 *
 * A counter's raw value is monotonic, so plotting it draws a line that only ever goes
 * up and says almost nothing; the rate is the quantity anyone actually wants.
 *
 * **This is a derived series and callers must say so.** The guest never emitted these
 * numbers. Presenting a computed rate as though it came off the wire would be the
 * chart telling a small lie about its own provenance, which is precisely what this
 * project's telemetry is for avoiding.
 *
 * Two cases that produce nonsense if unhandled:
 *
 * - **A repeated timestamp** divides by zero. Two samples can share a guest tick.
 * - **A counter reset** — a restarted guest, a re-registered counter — makes the
 *   delta negative, and a naive rate draws an enormous downward spike that looks
 *   like a real event. Treated as a reset and reported as zero, the way any
 *   time-series system handles it.
 */
export function rateOf(points: readonly [number, number][]): Point[] {
  const out: Point[] = [];
  for (let i = 1; i < points.length; i++) {
    const [t0, v0] = points[i - 1] as [number, number];
    const [t1, v1] = points[i] as [number, number];

    const dt = t1 - t0;
    if (dt <= 0) continue; // same tick, or time went backwards: no rate to speak of

    const dv = v1 - v0;
    const perSecond = dv < 0 ? 0 : dv / (dt / TIMEBASE_HZ);
    out.push({ t: t1, v: perSecond });
  }
  return out;
}

/** A metric's samples as chart points, already in the form its kind calls for. */
export function pointsFor(series: MetricSeries): Point[] {
  if (series.kind === "Counter") return rateOf(series.points);
  return series.points.map(([t, v]) => ({ t, v }));
}

/** What the y-axis is measuring, given the kind. */
export function unitFor(series: MetricSeries): string {
  return series.kind === "Counter" ? "per second (derived)" : "value";
}

/**
 * Whether this metric can be charted honestly.
 *
 * Histograms are excluded. `MetricKind::Histogram` is on the wire, and drawing a line
 * through one would be plotting *something* — a sum, a count — while labelling it
 * with the histogram's name, which misrepresents what the guest measured. Better
 * absent than misleading; they can be rendered properly later.
 */
export function chartable(series: MetricSeries): boolean {
  return series.kind !== "Histogram";
}

/**
 * The group a metric belongs to: the segment after the `snitchos.` prefix.
 *
 * `snitchos.heap.bytes_used` → `heap`. Derived from the name rather than curated by
 * hand, so a metric added to the guest appears in the page without anyone editing a
 * list here — the same reason the workload picker validates against the kernel's
 * registry instead of duplicating it.
 */
export function groupOf(name: string): string {
  const parts = name.split(".");
  if (parts[0] === "snitchos" && parts.length > 2) return parts[1] as string;
  return parts[0] ?? name;
}

/** One group of metrics, ready to render as small multiples. */
export interface MetricGroup {
  name: string;
  series: MetricSeries[];
}

/**
 * Group the chartable series by name, in first-seen order.
 *
 * Small multiples rather than one chart per group: a group mixes units — bytes beside
 * block counts in `heap` — and putting those on one axis is the dual-axis mistake
 * wearing a different hat. One chart per metric, laid out together, compares
 * shapes without claiming the scales are comparable.
 */
export function groupsOf(all: readonly MetricSeries[]): MetricGroup[] {
  const groups: MetricGroup[] = [];
  for (const series of all) {
    if (!chartable(series)) continue;
    const name = groupOf(series.name);
    const existing = groups.find((g) => g.name === name);
    if (existing) existing.series.push(series);
    else groups.push({ name, series: [series] });
  }
  return groups;
}

/** Strip the leading `snitchos.<group>.` so a chart title is readable in a small box. */
export function shortName(name: string): string {
  const parts = name.split(".");
  if (parts[0] === "snitchos" && parts.length > 2) return parts.slice(2).join(".");
  return name;
}
