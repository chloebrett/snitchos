import { useState } from "react";
import type { Views } from "./frames";
import { GraphView } from "./GraphView";
import type { Graph } from "./graph";
import { MetricsPanel } from "./MetricsPanel";
import { TelemetryPane } from "./TelemetryPane";
import type { FrameRow } from "./tail";

/** Which view the right-hand pane is showing. */
export type PanelId = "caps" | "spans" | "switches" | "metrics" | "frames";

/**
 * The folded views, each with what to say when it is empty.
 *
 * A table rather than a chain of conditionals: every entry here is the *same* kind of
 * thing — a graph and the sentence that explains its absence — and the next one
 * (metrics, once they land) is too. Adding a branch per view would put that sameness
 * behind a decision tree that grows a level each time.
 */
const GRAPH_TABS: Array<{
  id: PanelId;
  label: string;
  of: (v: Views) => Graph;
  empty: string;
}> = [
  {
    id: "caps",
    label: "capabilities",
    of: (v) => v.caps,
    empty: "no capabilities derived yet",
  },
  { id: "spans", label: "spans", of: (v) => v.spans, empty: "no spans yet" },
  {
    id: "switches",
    label: "switches",
    of: (v) => v.switches,
    empty: "no context switches yet",
  },
];

const TABS: Array<{ id: PanelId; label: string }> = [
  ...GRAPH_TABS.map(({ id, label }) => ({ id, label })),
  { id: "metrics" as const, label: "metrics" },
  { id: "frames" as const, label: "frames" },
];

/**
 * The structural views of a running guest, folded from its own telemetry.
 *
 * These are deliberately the things a general dashboard is bad at. Line charts are
 * Prometheus and Grafana's job and stay there; what nothing off-the-shelf will draw
 * is *this machine's own subject matter* — which capability came from which, which
 * span opened inside which, which task yielded to which.
 */
export function Panels({
  views,
  frames,
}: {
  views: Views | null;
  frames: readonly FrameRow[];
}) {
  const [active, setActive] = useState<PanelId>("caps");

  return (
    <section className="flex min-h-0 min-w-0 flex-1 flex-col rounded-md border border-neutral-800">
      <div className="flex shrink-0 items-center gap-1 border-neutral-800 border-b px-2 py-1">
        {TABS.map((tab) => (
          <button
            key={tab.id}
            type="button"
            data-testid={`tab-${tab.id}`}
            aria-pressed={active === tab.id}
            onClick={() => setActive(tab.id)}
            className={`rounded px-2 py-0.5 text-[0.68rem] uppercase tracking-wider ${
              active === tab.id
                ? "bg-neutral-800 text-neutral-200"
                : "text-neutral-500 hover:text-neutral-300"
            }`}
          >
            {tab.label}
          </button>
        ))}
        {views !== null && (
          // The unbounded bucket, in view rather than assumed. If this climbs without
          // limit on a long run, the "bounded in practice" assumption was wrong.
          <span
            data-testid="durable-count"
            className="ml-auto pr-1 text-[0.65rem] text-neutral-600 tabular-nums"
            title="cumulative frames retained (registrations and capability lifecycle)"
          >
            {views.durableFrames} kept
          </span>
        )}
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto">
        <Body active={active} views={views} frames={frames} />
      </div>
    </section>
  );
}

function Body({
  active,
  views,
  frames,
}: {
  active: PanelId;
  views: Views | null;
  frames: readonly FrameRow[];
}) {
  if (active === "frames") return <TelemetryPane rows={frames} />;
  if (active === "metrics") {
    return views === null ? (
      <p className="px-3 py-2 text-neutral-600 text-xs italic">waiting for the guest…</p>
    ) : (
      <MetricsPanel series={views.metrics} />
    );
  }

  // "No source yet" is not "a source that produced nothing". Conflating them shows an
  // empty capability tree during boot, which reads as *this guest granted nothing*.
  if (views === null) {
    return (
      <p className="px-3 py-2 text-neutral-600 text-xs italic">waiting for the guest…</p>
    );
  }

  const tab = GRAPH_TABS.find((t) => t.id === active);
  if (!tab) return null;
  return <GraphView graph={tab.of(views)} empty={tab.empty} />;
}
