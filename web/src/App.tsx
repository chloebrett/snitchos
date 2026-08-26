import { useCallback, useEffect, useRef, useState } from "react";
import { Console, type ConsoleHandle } from "./Console";
import { describe, type FrameView, type Status, type Views } from "./frames";
import { encodeInput } from "./input";
import { Panels } from "./Panels";
import { Pacer, type Speed } from "./pace";
import { progressLabel } from "./progress";
import { appendCapped, mips, Pump } from "./pump";
import {
  type BuildManifest,
  fetchKernel,
  SnemuSource,
  type Workload,
  workloads,
} from "./snemu";

/**
 * How often to re-fold the structural views, in milliseconds.
 *
 * Four times a second: fast enough that a capability grant appears while you are
 * still looking for it, slow enough that folding a full retention window is not
 * competing with the guest for the frame budget.
 */
const FOLD_INTERVAL_MS = 250;

export function App() {
  const [status, setStatus] = useState<Status | null>(null);
  const [frames, setFrames] = useState<readonly FrameView[]>([]);
  const [instret, setInstret] = useState(0);
  const [rate, setRate] = useState(0);
  const [manifest, setManifest] = useState<BuildManifest | null>(null);
  const [kernelBytes, setKernelBytes] = useState<number | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState<string | null>(null);
  const [views, setViews] = useState<Views | null>(null);
  const [speed, setSpeed] = useState<Speed>("paced");
  const [choices, setChoices] = useState<Workload[]>([]);
  const [workload, setWorkload] = useState("");

  // Read inside the animation-frame callback, which is created once — a ref keeps it
  // seeing the current choice without tearing down and restarting the guest.
  const speedRef = useRef<Speed>(speed);
  speedRef.current = speed;

  const term = useRef<ConsoleHandle | null>(null);
  const onConsoleReady = useCallback((h: ConsoleHandle) => {
    term.current = h;
  }, []);

  // Typed characters go straight to the guest, translated for its console. Held in a
  // ref so the guest can be swapped underneath without re-creating the terminal.
  const source = useRef<SnemuSource | null>(null);
  const onInput = useCallback((data: string) => {
    source.current?.pushInput(encodeInput(data));
  }, []);

  useEffect(() => {
    workloads()
      .then(setChoices)
      .catch(() => setChoices([]));
  }, []);

  useEffect(() => {
    let frame = 0;
    let cancelled = false;

    (async () => {
      try {
        setLoading(progressLabel(0, null));
        const { elf, manifest } = await fetchKernel((received, total) => {
          if (!cancelled) setLoading(progressLabel(received, total));
        });
        if (cancelled) return;
        setLoading(null);
        setManifest(manifest);
        setKernelBytes(elf.length);

        const booted = await SnemuSource.boot(elf, workload);
        if (cancelled) return;
        source.current = booted;
        const pump = new Pump(booted);
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
        // Re-fold on a cadence, not per frame. These are batch folds over the whole
        // retention window, and the structures they produce change on the timescale a
        // person reads them — a capability is granted, a task yields — not sixty times
        // a second. Folding per frame would spend the emulator's budget on redrawing
        // an unchanged tree.
        let nextFold = 0;
        const step = (now: number) => {
          const slice = pump.tick(
            pacer.budgetFor(speedRef.current, now - last, pump.instret),
          );
          last = now;
          if (slice) {
            if (slice.text) term.current?.write(slice.text);
            setFrames((prev) => appendCapped(prev, slice.frames));
            setStatus(slice.status);
            setInstret(slice.instret);
            setRate(mips(slice.instret, now - started));
          }

          if (now >= nextFold) {
            nextFold = now + FOLD_INTERVAL_MS;
            setViews(booted.views());
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
    // Re-runs on a workload change: the old guest's loop is cancelled and a fresh
    // machine boots. Everything derived from the old one is reset below.
  }, [workload]);

  /**
   * Switch workload, discarding the previous guest's output.
   *
   * The terminal and the telemetry tail both have to be cleared, and neither is React
   * state — the terminal owns its own scrollback. Leaving either would show the new
   * boot appended to the old one, which reads as a machine that rebooted itself.
   */
  const chooseWorkload = useCallback((next: string) => {
    setWorkload((current) => {
      if (next === current) return current; // selecting what is already running
      term.current?.clear();
      setFrames([]);
      setViews(null);
      setInstret(0);
      setRate(0);
      setStatus(null);
      return next;
    });
  }, []);

  const statusText = error ?? loading ?? (status ? describe(status) : "starting…");
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
        <select
          data-testid="workload"
          value={workload}
          onChange={(e) => chooseWorkload(e.target.value)}
          className="ml-auto rounded border border-neutral-700 bg-neutral-900 px-1.5 py-0.5 text-neutral-300 text-xs"
        >
          {choices.map((w) => (
            <option key={w.name} value={w.name}>
              {w.label}
            </option>
          ))}
        </select>
        <label className="flex items-center gap-1.5 text-neutral-500 text-xs">
          <input
            type="checkbox"
            data-testid="turbo"
            checked={speed === "turbo"}
            onChange={(e) => setSpeed(e.target.checked ? "turbo" : "paced")}
            className="accent-sky-400"
          />
          {/* Paced is the default: the guest's timers mean what they say, and the tab
              costs a fraction of a core. Turbo is for compute-bound work, where real
              time is the wrong master — a model completion is ~4x faster and the tab
              costs a whole core. */}
          turbo
        </label>
        <span className="text-neutral-500 text-xs tabular-nums">
          <b data-testid="instret" className="font-semibold text-neutral-200">
            {(instret / 1e6).toFixed(1)}M
          </b>{" "}
          instret · <b className="font-semibold text-neutral-200">{rate.toFixed(1)}</b>{" "}
          MIPS
        </span>
      </header>

      <main className="flex min-h-0 flex-1 gap-3">
        <Console onReady={onConsoleReady} onInput={onInput} />
        <Panels views={views} frames={frames} />
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
