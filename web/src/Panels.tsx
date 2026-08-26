import { useState } from "react";
import type { FrameView, Views } from "./frames";
import { GraphView } from "./GraphView";
import { TelemetryPane } from "./TelemetryPane";

/** Which view the right-hand pane is showing. */
export type PanelId = "caps" | "spans" | "switches" | "frames";

const TABS: Array<{ id: PanelId; label: string }> = [
  { id: "caps", label: "capabilities" },
  { id: "spans", label: "spans" },
  { id: "switches", label: "switches" },
  { id: "frames", label: "frames" },
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
  frames: readonly FrameView[];
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
        {active === "frames" ? (
          <TelemetryPane frames={frames} />
        ) : views === null ? (
          <p className="px-3 py-2 text-neutral-600 text-xs italic">
            waiting for the guest…
          </p>
        ) : active === "caps" ? (
          <GraphView graph={views.caps} empty="no capabilities derived yet" />
        ) : active === "spans" ? (
          <GraphView graph={views.spans} empty="no spans yet" />
        ) : (
          <GraphView graph={views.switches} empty="no context switches yet" />
        )}
      </div>
    </section>
  );
}
