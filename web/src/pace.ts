/**
 * Running the guest at wall-clock speed instead of as fast as possible.
 *
 * Without this the tab burns a whole core forever, which was measured rather than
 * guessed (`e2e/idle-cost.spec.ts`). The cause is not waste: snemu's clock *is* the
 * retired-instruction count, so one second of guest time costs
 * {@link TIMEBASE_HZ} instructions, and running flat out simply buys guest time as
 * fast as the host can make it. The fix is to buy only as much as has actually
 * elapsed, and then stop.
 *
 * That only became worthwhile once the emulator had headroom. At 11 MIPS — the
 * default interpreter — pacing to a 10 MHz timebase would have saved about 9%. With
 * snemu's caches enabled the same page measures 38.9 MIPS, so real time costs roughly
 * a quarter of a core and the rest becomes idle.
 */

/**
 * Guest timer ticks per second, and therefore guest instructions per second of real
 * time. QEMU `virt`'s timebase, which is what the DTB the guest boots with declares —
 * so this is a property of the machine we emulate, not a tuning knob.
 */
export const TIMEBASE_HZ = 10_000_000;

/**
 * The most guest instructions one animation frame may run.
 *
 * Only reached while catching up after a stall — in the steady state a 16ms frame
 * asks for ~160k. It bounds how long a single frame can block, which is the
 * difference between "briefly behind" and "the tab locked up". Roughly 13ms at the
 * measured 38.9 MIPS.
 */
export const MAX_INSTRET_PER_FRAME = 500_000;

/**
 * The most guest time the pacer will ever try to make up: 250ms.
 *
 * A host that cannot keep up would otherwise accumulate an unpayable backlog and
 * spend forever trying to burn through it — the classic game-loop spiral, where
 * falling behind makes you fall further behind. It is also what makes a hidden tab
 * safe: the browser stops calling `requestAnimationFrame`, and the multi-second gap
 * on return is forgiven rather than chased.
 */
export const MAX_DEBT_INSTRET = TIMEBASE_HZ / 4;

/**
 * Decides how much guest time to buy each frame.
 *
 * Driven by the *delta* since the previous frame rather than an absolute start time,
 * so a pause needs no special case: a long gap is just a large `dtMs`, which the debt
 * ceiling then forgives.
 */
export class Pacer {
  #timebaseHz: number;
  #maxPerFrame: number;
  #maxDebt: number;
  /** Guest instructions the guest *should* have retired by now. */
  #target = 0;

  constructor(
    timebaseHz: number = TIMEBASE_HZ,
    maxPerFrame: number = MAX_INSTRET_PER_FRAME,
    maxDebt: number = MAX_DEBT_INSTRET,
  ) {
    this.#timebaseHz = timebaseHz;
    this.#maxPerFrame = maxPerFrame;
    this.#maxDebt = maxDebt;
  }

  /** How far behind real time the guest currently is, in guest instructions. */
  get debt(): number {
    return this.#debt;
  }
  #debt = 0;

  /**
   * Advance the target by `dtMs` of wall clock and say how much to run now.
   *
   * Returns 0 when the guest has caught up — that is the whole point, and it is what
   * lets the core go idle between frames.
   *
   * **Always a whole number.** Instructions are discrete, and the value crosses into
   * wasm through `BigInt(...)`, which *throws* on a fractional argument rather than
   * rounding. A 16ms frame at 10 MHz is 166666.67 instructions, so this is the
   * common case, not an edge one — an unfloored budget stopped the guest booting at
   * all. The running target keeps its fractional part, so flooring here costs
   * precision on one frame rather than accumulating drift.
   */
  budget(dtMs: number, instretSoFar: number): number {
    // A negative or absent delta (a clock that jumped backwards, a first frame)
    // buys nothing rather than something arbitrary.
    if (dtMs > 0) {
      this.#target += (dtMs / 1000) * this.#timebaseHz;
    }

    // Forgive anything beyond the ceiling before it can compound.
    if (this.#target - instretSoFar > this.#maxDebt) {
      this.#target = instretSoFar + this.#maxDebt;
    }

    this.#debt = Math.max(0, this.#target - instretSoFar);
    return Math.floor(Math.min(this.#debt, this.#maxPerFrame));
  }
}
