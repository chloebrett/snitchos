import { useEffect, useRef, useState } from "react";
import { type FrameRow, kindsIn, visibleRows } from "./tail";

interface Props {
  rows: readonly FrameRow[];
}

/**
 * The live tail of decoded telemetry.
 *
 * Runs of like frames arrive already collapsed (see `tail.ts`), so a burst of
 * thousands of context switches is one counted row rather than a wall of them. Kinds
 * can also be hidden outright — the DevTools move — for when you want to watch one
 * thing rather than summarise everything.
 *
 * No frame or heading of its own: `Panels` supplies both, and nesting a second
 * bordered box with a second title inside the first was a leftover from before the
 * tabs existed.
 */
export function TelemetryPane({ rows }: Props) {
  const scroller = useRef<HTMLDivElement>(null);
  const pinned = useRef(true);
  const [hidden, setHidden] = useState<ReadonlySet<string>>(new Set());

  // Depend on the count rather than the array: it is what actually changes when
  // telemetry arrives, and it is genuinely read here, so the dependency is real
  // rather than a trigger the linter has to be told to tolerate.
  const count = rows.length;
  useEffect(() => {
    const el = scroller.current;
    if (el && pinned.current && count > 0) el.scrollTop = el.scrollHeight;
  }, [count]);

  function onScroll() {
    const el = scroller.current;
    if (!el) return;
    // Follow the newest row only while the reader is already at the bottom:
    // scrolling up to read something must not be undone by the next frame arriving.
    pinned.current = el.scrollTop + el.clientHeight >= el.scrollHeight - 24;
  }

  function toggle(kind: string) {
    setHidden((current) => {
      const next = new Set(current);
      if (!next.delete(kind)) next.add(kind);
      return next;
    });
  }

  const kinds = kindsIn(rows);
  const shown = visibleRows(rows, hidden);

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      {kinds.length > 0 && (
        <div
          className="flex shrink-0 flex-wrap gap-1 border-neutral-800/60 border-b px-2 py-1"
          data-testid="kind-filters"
        >
          {kinds.map((kind) => (
            <button
              key={kind}
              type="button"
              data-testid={`filter-${kind}`}
              aria-pressed={!hidden.has(kind)}
              onClick={() => toggle(kind)}
              className={`rounded px-1.5 py-px text-[0.62rem] ${
                hidden.has(kind)
                  ? "text-neutral-700 line-through"
                  : "bg-neutral-800/70 text-neutral-400"
              }`}
            >
              {kind}
            </button>
          ))}
        </div>
      )}

      <div
        ref={scroller}
        onScroll={onScroll}
        data-testid="frame-list"
        className="min-h-0 flex-1 overflow-y-auto px-3 py-2 font-mono text-[0.72rem]"
      >
        {shown.map((row, i) => (
          // Frames are an append-only stream with no identity of their own, and the
          // list is capped from the front, so the index is the honest key here.
          // biome-ignore lint/suspicious/noArrayIndexKey: append-only stream
          <div key={i} className="flex gap-2 py-px" data-testid="frame-row">
            <span className="w-32 shrink-0 truncate text-neutral-500">
              {row.view.kind}
              {/* The count *is* the signal: "500 switches happened here" says more
                  than 500 identical rows, and more than hiding them would. */}
              {row.count > 1 && (
                <span className="ml-1 text-neutral-600 tabular-nums">×{row.count}</span>
              )}
            </span>
            <span className="truncate text-sky-400">{row.view.name ?? ""}</span>
            {row.view.value !== null && (
              <span className="ml-auto shrink-0 text-amber-400 tabular-nums">
                {row.view.value}
              </span>
            )}
          </div>
        ))}
      </div>
    </div>
  );
}
