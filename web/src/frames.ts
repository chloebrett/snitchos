/**
 * The app's vocabulary: what a telemetry frame looks like once it reaches the UI,
 * and where a stream of them can come from.
 *
 * The four sources named in `docs/` — the emulator in this tab, a host socket, a
 * board over serial, and replay of a recorded stream — are one interface because the
 * wire is one interface. Only the first is implemented; the point of declaring
 * `FrameSource` now is that the boot page is written against it rather than against
 * the emulator, so adding the others is an implementation, not a refactor.
 */

import type { Graph as GraphData } from "./graph";
import type { MetricSeries } from "./metrics";

/** Mirrors `snemu_wasm::telemetry::FrameView`. Its shape is pinned Rust-side. */
export interface FrameView {
  /** The wire variant, e.g. `"SpanStart"`. */
  kind: string;
  /**
   * The interned name, resolved through the stream's string table.
   *
   * `null` means the frame carries no name *or* cites a `StringId` that has not been
   * registered yet — deliberately not a placeholder string, so the UI can show the
   * difference rather than assert a name it does not have.
   */
  name: string | null;
  /** Guest timestamp, for frames that carry one. */
  t: number | null;
  /** A metric's value. */
  value: number | null;
}

/** Mirrors `snemu_wasm::budget::Status`; serde's externally-tagged encoding. */
export type Status =
  | { Running: { instret: number } }
  | { Halted: { instret: number } }
  | { Trapped: { instret: number; reason: string } };

/** What one slice of running produced. */
export interface Slice {
  status: Status;
  /** UART bytes as text, ready for `term.write()`. */
  text: string;
  frames: FrameView[];
  /** The source's cumulative clock, in guest instructions. */
  instret: number;
  /**
   * Whether the guest had nothing to do at the end of this slice.
   *
   * Drives pacing: an idle guest is held to real time (which is what stops a tab
   * burning a core), a busy one runs as fast as the host allows (which is what keeps
   * a compute-bound completion from taking 42 seconds). A source with no notion of
   * idleness — a socket, a replay — reports `false` and is simply never throttled.
   */
  idle: boolean;
}

/**
 * Anything that can produce frames and console text incrementally.
 *
 * `advance` is deliberately *pull*-based and bounded: the caller decides how much
 * work to do and when, which is what lets a browser stay responsive while it happens.
 * A socket- or serial-backed source ignores the budget and returns whatever has
 * arrived; an emulator source runs that many instructions.
 */
export interface FrameSource {
  /** Do up to `budget` units of work and return everything new since last time. */
  advance(budget: number): Slice;
  /** A short description of where these frames come from, for the UI. */
  readonly label: string;
  /**
   * Deliver typed characters to the source, if it accepts any.
   *
   * Optional because not every source is interactive: a replay has nothing to type
   * at, and a read-only socket may not either. The page hides its keyboard wiring
   * when this is absent rather than pretending input went somewhere.
   */
  pushInput?(text: string): void;

  /**
   * The structural views folded from everything this source has seen.
   *
   * Optional for the same reason `pushInput` is: a source that only relays text has
   * no frames to fold. A panel with no source to ask says so rather than rendering an
   * empty graph, which would read as "the guest granted nothing".
   */
  views?(): Views;
}

/** The folded views a panel renders. */
export interface Views {
  /** Who granted which capability to whom. */
  caps: GraphData;
  /** Which span opened inside which. */
  spans: GraphData;
  /** Context-switch transitions between tasks. */
  switches: GraphData;
  /** Every metric's history, for the charts. */
  metrics: MetricSeries[];
  /**
   * How many cumulative facts the source is holding.
   *
   * That bucket has no ceiling by design, so it is surfaced rather than trusted —
   * "bounded in practice" is an assumption about guest behaviour.
   */
  durableFrames: number;
}

/** Whether a status means there is any point scheduling another slice. */
export function isTerminal(status: Status): boolean {
  return !("Running" in status);
}

/** A one-line description of a status, for the status line. */
export function describe(status: Status): string {
  if ("Running" in status) return "running";
  if ("Halted" in status) return "halted — the guest cannot make further progress";
  return `trapped: ${status.Trapped.reason}`;
}
