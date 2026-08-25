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
 * Guest instructions per animation frame.
 *
 * Boot is roughly 25M instructions, so ~2M per frame reaches heartbeat in a second
 * or two while leaving each frame near the 16ms budget on a desktop machine. It is a
 * comfort knob, not a correctness one: too high and the tab janks, too low and boot
 * crawls, but neither changes what the guest computes.
 */
export const INSTRET_PER_FRAME = 2_000_000;

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
  #budget: number;
  #done = false;

  constructor(source: FrameSource, budget: number = INSTRET_PER_FRAME) {
    this.#source = source;
    this.#budget = budget;
  }

  /** True once the guest reached a terminal state; further ticks do nothing. */
  get done(): boolean {
    return this.#done;
  }

  /**
   * Advance by one slice, or return `null` if the guest has already finished.
   *
   * Refusing to step a finished guest is the point: a `Halted` machine would
   * otherwise be asked to run again on every animation frame for as long as the tab
   * is open, burning a core to retire nothing.
   */
  tick(): Slice | null {
    if (this.#done) return null;
    const slice = this.#source.advance(this.#budget);
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
