import { useCallback, useEffect, useRef, useState } from "react";
import { Console } from "./Console";
import { describe, type FrameView, type Status } from "./frames";
import { Pacer } from "./pace";
import { appendCapped, mips, Pump } from "./pump";
import { type BuildManifest, fetchKernel, SnemuSource } from "./snemu";
import { TelemetryPane } from "./TelemetryPane";

export function App() {
  const [status, setStatus] = useState<Status | null>(null);
  const [frames, setFrames] = useState<readonly FrameView[]>([]);
  const [instret, setInstret] = useState(0);
  const [rate, setRate] = useState(0);
  const [manifest, setManifest] = useState<BuildManifest | null>(null);
  const [kernelBytes, setKernelBytes] = useState<number | null>(null);
  const [error, setError] = useState<string | null>(null);

  const write = useRef<(text: string) => void>(() => {});
  const onConsoleReady = useCallback((w: (text: string) => void) => {
    write.current = w;
  }, []);

  useEffect(() => {
    let frame = 0;
    let cancelled = false;

    (async () => {
      try {
        const { elf, manifest } = await fetchKernel();
        if (cancelled) return;
        setManifest(manifest);
        setKernelBytes(elf.length);

        const pump = new Pump(await SnemuSource.boot(elf));
        if (cancelled) return;
        const pacer = new Pacer();
        const started = performance.now();
        let last = started;

        // One paced slice per animation frame.
        //
        // The budget is what the *clock* says is owed, not what the host could
        // manage: running flat out buys guest time as fast as the machine can make
        // it, which measured as 100% of a core forever. Asking the pacer instead
        // means most frames in the steady state do nothing at all.
        //
        // Still bounded per frame, for the other reason `step_budget` takes a
        // budget: a `while` loop to completion would freeze the tab for a whole
        // boot.
        const step = (now: number) => {
          const slice = pump.tick(pacer.budget(now - last, pump.instret));
          last = now;
          if (slice) {
            if (slice.text) write.current(slice.text);
            setFrames((prev) => appendCapped(prev, slice.frames));
            setStatus(slice.status);
            setInstret(slice.instret);
            setRate(mips(slice.instret, now - started));
          }
          // Keep scheduling even once done, so a future source (or a restart) has a
          // running clock to attach to; `tick` is a no-op past the terminal state.
          frame = requestAnimationFrame(step);
        };
        frame = requestAnimationFrame(step);
      } catch (e) {
        if (!cancelled) setError(e instanceof Error ? e.message : String(e));
      }
    })();

    return () => {
      cancelled = true;
      cancelAnimationFrame(frame);
    };
  }, []);

  const statusText = error ?? (status ? describe(status) : "loading…");
  const statusTone = error
    ? "text-rose-400"
    : status && "Trapped" in status
      ? "text-rose-400"
      : status && "Halted" in status
        ? "text-amber-400"
        : "text-neutral-500";

  return (
    <div className="flex h-full flex-col gap-3 p-3 font-mono text-neutral-200">
      <header className="flex shrink-0 items-baseline gap-3">
        {/* Purely a liveness tell: if the rAF loop ever blocks, this visibly stops,
            which is how you tell "still booting" from "the tab is wedged". */}
        <div
          data-testid="heartbeat"
          className="size-2 shrink-0 animate-pulse rounded-full bg-sky-400"
        />
        <h1 className="font-semibold text-sm">
          SnitchOS <span className="font-normal text-neutral-500">· snemu · wasm32</span>
        </h1>
        <span data-testid="status" className={`text-xs ${statusTone}`}>
          {statusText}
        </span>
        <span className="ml-auto text-neutral-500 text-xs tabular-nums">
          <b data-testid="instret" className="font-semibold text-neutral-200">
            {(instret / 1e6).toFixed(1)}M
          </b>{" "}
          instret · <b className="font-semibold text-neutral-200">{rate.toFixed(1)}</b>{" "}
          MIPS
        </span>
      </header>

      <main className="flex min-h-0 flex-1 gap-3">
        <Console onReady={onConsoleReady} />
        <TelemetryPane frames={frames} />
      </main>

      <footer data-testid="build" className="shrink-0 text-[0.7rem] text-neutral-600">
        {kernelBytes === null
          ? "kernel: —"
          : `kernel: ${kernelBytes.toLocaleString()} bytes` +
            (manifest
              ? ` · fingerprint ${manifest.kernel_fingerprint} · git ${manifest.git_rev}`
              : " · no build.json")}
      </footer>
    </div>
  );
}
