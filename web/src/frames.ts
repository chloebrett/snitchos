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
