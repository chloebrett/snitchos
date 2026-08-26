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
 * How fast to run the guest.
 *
 * - `paced` — real time. The guest's clock matches the wall clock, so its timers are
 *   truthful, and the tab costs a fraction of a core.
 * - `turbo` — as fast as the host manages. For a compute-bound workload this is the
 *   difference between a demo and a stopwatch, and it costs a whole core.
 */
export type Speed = "paced" | "turbo";

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
  /**
   * The budget for a frame under a chosen {@link Speed}.
   *
   * `Paced` holds the guest to real time: cheap, and the guest's timers mean what
   * they say. `Turbo` runs it as fast as the host allows, which is the difference
   * between a ~42-second Tab completion from the trained model and a ~11-second one.
   *
   * **This was originally driven by whether the guest was idle**, on the theory that
   * an idle guest should be paced and a working one let run. Measured, the theory
   * did not survive: SnitchOS's idle task is `loop { wfi; yield_now(); }`, so it
   * retires instructions between waits instead of parking, and a booted guest logged
   * **zero** clock fast-forwards over 60M instructions. There is no idle state here
   * to detect — the guest is always busy, and an idle-driven rule would simply
   * always choose full speed. So the choice is the user's, honestly.
   *
   * Turbo advances the guest's clock far past the wall clock, which is why
   * {@link budget} clamps credit: without that, switching back to `Paced` after a
   * long run would leave tens of seconds of credit and freeze the guest until real
   * time caught up — a worse fault than the one being avoided.
   */
  budgetFor(speed: Speed, dtMs: number, instretSoFar: number): number {
    if (speed === "turbo") {
      // No target bookkeeping here: the frame about to run will push the guest
      // further ahead than anything predicted, and `budget`'s credit clamp re-anchors
      // on the next idle frame from what actually happened rather than a guess.
      this.#debt = 0;
      return this.#maxPerFrame;
    }
    return this.budget(dtMs, instretSoFar);
  }

  budget(dtMs: number, instretSoFar: number): number {
    // Never carry *credit*: guest time already spent is not owed back.
    //
    // The debt ceiling's mirror, and load-bearing rather than tidy. A frame that ran
    // flat out (see `budgetFor`) can put the guest tens of seconds ahead of the wall
    // clock, and a pacer treating that as credit would freeze the guest until real
    // time caught up — trading a busy tab for a frozen one. It also absorbs the
    // ordinary case: a slice overshoots its budget because a step is atomic, and
    // repaying that by starving the next frame would be pacing to a precision the
    // clock does not have.
    //
    // **Before** the clock advances, not after: clamping afterwards would discard
    // the time this very frame just bought, and the guest would never run again.
    if (this.#target < instretSoFar) {
      this.#target = instretSoFar;
    }

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
