/**
 * Driving a {@link FrameSource} in bounded slices, and the small pieces of
 * arithmetic the UI needs around it.
 *
 * Deliberately plain TypeScript with no React and no `requestAnimationFrame`: the
 * decisions here (has the guest finished? how many rows do we keep? what is the
 * throughput?) are testable, and the scheduling that wraps them is not. The React
 * hook supplies the clock; this file supplies the judgement.
 */

import { type FrameSource, type FrameView, isTerminal, type Slice } from "./frames";

/**
 * How many telemetry rows the pane keeps.
 *
 * A live tail, not a log. An unbounded list grows without limit across a long boot
 * and takes the frame rate down with it — the DOM, not the emulator, becomes the
 * bottleneck.
 */
export const MAX_FRAME_ROWS = 400;

/** Runs a source in slices and knows when to stop. */
export class Pump {
  #source: FrameSource;
  #done = false;

  constructor(source: FrameSource) {
    this.#source = source;
  }

  /** True once the guest reached a terminal state; further ticks do nothing. */
  get done(): boolean {
    return this.#done;
  }

  /**
   * The guest's cumulative retired-instruction count, as of the last slice.
   *
   * This is what {@link Pacer} compares against the clock, so it has to be the
   * *guest's* count rather than a total of what was asked for: a slice may overshoot
   * its budget (a step is atomic, and a JIT block or a collapsed `memset` retires
   * many instructions at once), and pacing against the request rather than the
   * result would let that overshoot accumulate as phantom debt.
   */
  get instret(): number {
    return this.#instret;
  }
  #instret = 0;

  /**
   * Advance by up to `budget` guest instructions, or return `null` if there is
   * nothing to do.
   *
   * Two ways there is nothing to do, and both matter for a page left open:
   *
   * - The guest has finished. A `Halted` machine asked to run again on every
   *   animation frame would burn a core to retire nothing.
   * - The budget is zero, which is what {@link Pacer} returns once the guest has
   *   caught up with real time. Crossing into wasm to run no instructions is pure
   *   overhead, and it happens most frames in the steady state.
   */
  tick(budget: number): Slice | null {
    if (this.#done || budget <= 0) return null;
    const slice = this.#source.advance(budget);
    this.#instret = slice.instret;
    if (isTerminal(slice.status)) this.#done = true;
    return slice;
  }
}

/**
 * Append `incoming` to `existing`, keeping at most `cap` of the most recent.
 *
 * Returns `existing` unchanged when there is nothing new, so React can skip a
 * re-render on the many frames that produce no telemetry at all.
 */
export function appendCapped(
  existing: readonly FrameView[],
  incoming: readonly FrameView[],
  cap: number = MAX_FRAME_ROWS,
): readonly FrameView[] {
  if (incoming.length === 0) return existing;
  const joined = [...existing, ...incoming];
  return joined.length <= cap ? joined : joined.slice(joined.length - cap);
}

/** Millions of guest instructions per second of wall clock, or 0 before any elapse. */
export function mips(instret: number, elapsedMs: number): number {
  if (elapsedMs <= 0) return 0;
  return instret / 1e6 / (elapsedMs / 1000);
}
