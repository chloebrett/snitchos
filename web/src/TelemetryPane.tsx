import { useEffect, useRef } from "react";
import type { FrameView } from "./frames";

interface Props {
  frames: readonly FrameView[];
}

/**
 * The live tail of decoded telemetry.
 *
 * Follows the newest row only while the reader is already at the bottom — scrolling
 * up to read something must not be undone by the next frame arriving.
 */
export function TelemetryPane({ frames }: Props) {
  const scroller = useRef<HTMLDivElement>(null);
  const pinned = useRef(true);

  // Depend on the count rather than the array: it is what actually changes when
  // telemetry arrives, and it is genuinely read here, so the dependency is real
  // rather than a trigger the linter has to be told to tolerate.
  const count = frames.length;
  useEffect(() => {
    const el = scroller.current;
    if (el && pinned.current && count > 0) el.scrollTop = el.scrollHeight;
  }, [count]);

  function onScroll() {
    const el = scroller.current;
    if (!el) return;
    pinned.current = el.scrollTop + el.clientHeight >= el.scrollHeight - 24;
  }

  return (
    <section className="flex min-h-0 min-w-0 flex-1 flex-col rounded-md border border-neutral-800">
      <h2 className="border-neutral-800 border-b px-3 py-2 font-semibold text-[0.65rem] text-neutral-500 uppercase tracking-widest">
        Telemetry
        <span className="ml-2 tabular-nums lowercase tracking-normal">
          {frames.length} frame{frames.length === 1 ? "" : "s"}
        </span>
      </h2>
      <div
        ref={scroller}
        onScroll={onScroll}
        data-testid="frame-list"
        className="min-h-0 flex-1 overflow-y-auto px-3 py-2 font-mono text-[0.72rem]"
      >
        {frames.map((f, i) => (
          // Frames are an append-only stream with no identity of their own, and the
          // list is capped from the front, so the index is the honest key here.
          // biome-ignore lint/suspicious/noArrayIndexKey: append-only stream
          <div key={i} className="flex gap-2 py-px" data-testid="frame-row">
            <span className="w-32 shrink-0 truncate text-neutral-500">{f.kind}</span>
            <span className="truncate text-sky-400">{f.name ?? ""}</span>
            {f.value !== null && (
              <span className="ml-auto shrink-0 tabular-nums text-amber-400">
                {f.value}
              </span>
            )}
          </div>
        ))}
      </div>
    </section>
  );
}
