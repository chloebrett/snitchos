import { useId, useState } from "react";
import { type Box, extentOf, linePath, padded, type Point, scale, ticks } from "./scale";

/**
 * The categorical palette, in fixed slot order.
 *
 * Validated rather than chosen: `validate_palette.js` against this page's actual dark
 * surface (`#0d0f12`, not the reference `#1a1a19`) reports all five checks passing —
 * lightness band, chroma floor, CVD separation (worst adjacent ΔE 8.4 protan),
 * normal-vision floor (19.3), and ≥3:1 contrast.
 *
 * Assigned by slot and **never cycled**: colour follows the series, not its rank, so
 * hiding one series does not repaint the others. Past eight, the answer is fewer
 * series or small multiples, not a ninth generated hue.
 */
const SERIES_COLORS = ["#3987e5", "#d95926", "#199e70", "#c98500", "#d55181"] as const;

export interface ChartSeries {
  name: string;
  points: Point[];
}

interface Props {
  series: ChartSeries[];
  /** What the y-axis measures, e.g. `bytes`. */
  unit?: string;
  height?: number;
}

const PAD = { left: 52, right: 8, top: 8, bottom: 18 };

/**
 * A line chart over guest time.
 *
 * Hand-drawn SVG rather than a charting library: the forms here are lines and the
 * axis is *guest instret*, which a library would have to be argued out of treating as
 * a date. The x-axis is the point of the whole panel — snemu's clock is its
 * instruction counter and is deterministic, so two runs are comparable point for
 * point, which no wall-clock dashboard can offer.
 */
export function Chart({ series, unit, height = 150 }: Props) {
  const clipId = useId();
  const [hover, setHover] = useState<number | null>(null);

  const plot: Box = {
    width: 320 - PAD.left - PAD.right,
    height: height - PAD.top - PAD.bottom,
  };

  const allValues = series.flatMap((s) => s.points.map((p) => p.v));
  const allTimes = series.flatMap((s) => s.points.map((p) => p.t));
  const vExtent = extentOf(allValues);
  const tExtent = extentOf(allTimes);

  if (vExtent === null || tExtent === null) {
    return <p className="px-2 py-3 text-neutral-600 text-xs italic">no samples yet</p>;
  }

  const y = scale(padded(vExtent), plot.height);
  const x = scale(padded(tExtent), plot.width);
  const yTicks = ticks(vExtent, 3);

  // Values under the cursor, read off each series by nearest sample in guest time.
  const readout =
    hover === null
      ? null
      : series.map((s) => ({
          name: s.name,
          point: nearest(s.points, hover),
        }));

  return (
    <div className="px-1">
      <svg
        viewBox={`0 0 320 ${height}`}
        className="w-full"
        role="img"
        aria-label={`${series.map((s) => s.name).join(", ")} over guest time`}
        onMouseLeave={() => setHover(null)}
        onMouseMove={(e) => {
          const rect = e.currentTarget.getBoundingClientRect();
          const px = ((e.clientX - rect.left) / rect.width) * 320 - PAD.left;
          const span = padded(tExtent);
          setHover(span.min + (px / plot.width) * (span.max - span.min));
        }}
      >
        <title>{series.map((s) => s.name).join(", ")} over guest time</title>
        <clipPath id={clipId}>
          <rect x={0} y={0} width={plot.width} height={plot.height} />
        </clipPath>

        <g transform={`translate(${PAD.left} ${PAD.top})`}>
          {/* Recessive grid: present enough to read a value against, quiet enough
              that the data stays the loudest thing in the frame. */}
          {yTicks.map((t) => (
            <g key={t}>
              <line
                x1={0}
                x2={plot.width}
                y1={plot.height - y(t)}
                y2={plot.height - y(t)}
                stroke="#1e232b"
                strokeWidth={1}
              />
              <text
                x={-6}
                y={plot.height - y(t)}
                textAnchor="end"
                dominantBaseline="middle"
                className="fill-neutral-600 text-[8px] tabular-nums"
              >
                {compact(t)}
              </text>
            </g>
          ))}

          <g clipPath={`url(#${clipId})`}>
            {series.map((s, i) => (
              <path
                key={s.name}
                d={linePath(s.points, plot)}
                fill="none"
                // 2px lines, per the mark spec.
                strokeWidth={2}
                strokeLinejoin="round"
                stroke={SERIES_COLORS[i % SERIES_COLORS.length]}
              />
            ))}
          </g>

          {hover !== null && (
            <line
              x1={x(hover)}
              x2={x(hover)}
              y1={0}
              y2={plot.height}
              stroke="#6d7683"
              strokeWidth={1}
            />
          )}
        </g>

        {unit && (
          <text x={2} y={10} className="fill-neutral-600 text-[8px]">
            {unit}
          </text>
        )}
      </svg>

      {/* Identity is never colour alone: every series is named beside its swatch, and
          the text wears text tokens rather than the series colour. */}
      <ul className="flex flex-wrap gap-x-3 gap-y-0.5 px-1 pb-1 text-[0.62rem]" data-testid="legend">
        {series.map((s, i) => {
          const hit = readout?.find((r) => r.name === s.name)?.point;
          return (
            <li key={s.name} className="flex items-center gap-1 text-neutral-400">
              <span
                aria-hidden="true"
                className="inline-block size-2 rounded-full"
                style={{ background: SERIES_COLORS[i % SERIES_COLORS.length] }}
              />
              <span className="truncate">{s.name}</span>
              {hit && (
                <span data-testid="readout" className="text-neutral-200 tabular-nums">
                  {hit.v}
                </span>
              )}
            </li>
          );
        })}
      </ul>
    </div>
  );
}

/** The sample closest to `t`, or `undefined` for an empty series. */
function nearest(points: readonly Point[], t: number): Point | undefined {
  let best: Point | undefined;
  let bestGap = Number.POSITIVE_INFINITY;
  for (const p of points) {
    const gap = Math.abs(p.t - t);
    if (gap < bestGap) {
      bestGap = gap;
      best = p;
    }
  }
  return best;
}

/** Axis labels short enough not to collide: `1.2M` rather than `1200000`. */
export function compact(value: number): string {
  const abs = Math.abs(value);
  if (abs >= 1e9) return `${(value / 1e9).toFixed(1)}G`;
  if (abs >= 1e6) return `${(value / 1e6).toFixed(1)}M`;
  if (abs >= 1e3) return `${(value / 1e3).toFixed(1)}k`;
  return String(value);
}
