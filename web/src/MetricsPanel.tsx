import { useState } from "react";
import { Chart } from "./Chart";
import { groupsOf, type MetricSeries, pointsFor, shortName, unitFor } from "./metrics";

interface Props {
  series: readonly MetricSeries[];
}

/**
 * The guest's numbers, grouped and plotted over guest time.
 *
 * **Small multiples, not one chart per group.** A group mixes units — bytes beside
 * block counts in `heap` — and putting those on a shared axis is the dual-axis
 * mistake wearing a different hat. A grid of small charts compares *shapes* without
 * ever claiming the scales are comparable.
 *
 * One group is rendered at a time. Sixty charts is not a dashboard, and it is also
 * sixty re-renders per fold.
 */
export function MetricsPanel({ series }: Props) {
  const groups = groupsOf(series);
  const [active, setActive] = useState<string | null>(null);

  if (groups.length === 0) {
    return <p className="px-3 py-2 text-neutral-600 text-xs italic">no metrics yet</p>;
  }

  // Default to the first group once metrics arrive, without pinning that choice
  // before there is anything to choose between.
  const selected = groups.find((g) => g.name === active) ?? groups[0];

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div
        className="flex shrink-0 flex-wrap gap-1 border-neutral-800/60 border-b px-2 py-1"
        data-testid="metric-groups"
      >
        {groups.map((g) => (
          <button
            key={g.name}
            type="button"
            data-testid={`group-${g.name}`}
            aria-pressed={g.name === selected?.name}
            onClick={() => setActive(g.name)}
            className={`rounded px-1.5 py-px text-[0.62rem] ${
              g.name === selected?.name
                ? "bg-neutral-800/70 text-neutral-300"
                : "text-neutral-600 hover:text-neutral-400"
            }`}
          >
            {g.name}
            <span className="ml-1 text-neutral-600 tabular-nums">{g.series.length}</span>
          </button>
        ))}
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto" data-testid="metric-charts">
        {selected?.series.map((s) => (
          <figure key={s.name} className="border-neutral-800/40 border-b px-1 py-2">
            <figcaption className="px-1 pb-1 text-[0.66rem] text-neutral-400">
              {shortName(s.name)}
            </figcaption>
            <Chart
              series={[{ name: shortName(s.name), points: pointsFor(s) }]}
              unit={unitFor(s)}
              height={110}
            />
          </figure>
        ))}
      </div>
    </div>
  );
}
