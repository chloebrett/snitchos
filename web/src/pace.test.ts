import { describe, expect, it } from "vitest";
import { MAX_DEBT_INSTRET, MAX_INSTRET_PER_FRAME, Pacer, TIMEBASE_HZ } from "./pace";

/** One 60fps frame, in milliseconds. */
const FRAME_MS = 1000 / 60;
/** Guest instructions one such frame is worth at the real timebase. */
const FRAME_INSTRET = (FRAME_MS / 1000) * TIMEBASE_HZ;

describe("Pacer", () => {
  it("buys one frame's worth of guest time per frame", () => {
    const pacer = new Pacer();
    // Whole instructions, so the fractional remainder of a 16.67ms frame stays owed
    // rather than being asserted away.
    expect(pacer.budget(FRAME_MS, 0)).toBe(Math.floor(FRAME_INSTRET));
  });

  /**
   * The property the whole module exists for: a guest that has kept up gets nothing
   * to do, and the core goes idle. Before pacing this returned a full slice forever,
   * which measured as 100% of a core.
   */
  it("buys nothing when the guest has kept up", () => {
    const pacer = new Pacer();
    const first = pacer.budget(FRAME_MS, 0);
    expect(pacer.budget(0, first)).toBe(0);
  });

  it("buys nothing when the guest is ahead of real time", () => {
    const pacer = new Pacer();
    expect(pacer.budget(FRAME_MS, FRAME_INSTRET * 10)).toBe(0);
  });

  /** Falling a little behind is made up on the next frame, not dropped. */
  it("carries a small shortfall into the next frame", () => {
    const pacer = new Pacer();
    pacer.budget(FRAME_MS, 0); // asked for a frame's worth…
    const next = pacer.budget(FRAME_MS, FRAME_INSTRET / 2); // …but only half ran
    expect(next).toBeCloseTo(FRAME_INSTRET * 1.5, 0);
  });

  /**
   * One frame may never block for an unbounded time. Catching up is worth doing;
   * doing it all at once is how a page stops responding.
   */
  it("never asks for more than one frame's cap at a time", () => {
    const pacer = new Pacer();
    expect(pacer.budget(10_000, 0)).toBe(MAX_INSTRET_PER_FRAME);
  });

  /**
   * The spiral guard. A host that cannot keep up must not accumulate a backlog it
   * will chase forever — falling behind would make it fall further behind.
   */
  it("forgives debt beyond the ceiling instead of compounding it", () => {
    const pacer = new Pacer();
    // Ten seconds of wall clock with the guest frozen: a 100M-instruction debt if
    // it were all owed.
    pacer.budget(10_000, 0);
    expect(pacer.debt).toBe(MAX_DEBT_INSTRET);

    // And it stays forgiven rather than reappearing once the guest catches up.
    pacer.budget(0, MAX_DEBT_INSTRET);
    expect(pacer.debt).toBe(0);
  });

  /**
   * A hidden tab gets no animation frames, so returning to it produces one enormous
   * delta. That must not become a burst of catch-up work — it is the same spiral,
   * arriving all at once.
   */
  it("treats a long pause as a pause, not a backlog", () => {
    const pacer = new Pacer();
    pacer.budget(FRAME_MS, 0);

    const afterPause = pacer.budget(60_000, FRAME_INSTRET);
    expect(afterPause).toBe(MAX_INSTRET_PER_FRAME);
    // A minute of real time is 600M instructions; only the ceiling is owed.
    expect(pacer.debt).toBe(MAX_DEBT_INSTRET);
  });

  /** A zero or backwards delta buys nothing rather than something arbitrary. */
  it("ignores a non-advancing clock", () => {
    const pacer = new Pacer();
    expect(pacer.budget(0, 0)).toBe(0);
    expect(pacer.budget(-5, 0)).toBe(0);
  });

  /**
   * Sustained real-time operation costs the timebase per second and no more — the
   * claim that makes the CPU saving real rather than incidental.
   */
  it("costs exactly the timebase per second of wall clock", () => {
    const pacer = new Pacer();
    let instret = 0;
    for (let frame = 0; frame < 60; frame++) {
      instret += pacer.budget(FRAME_MS, instret);
    }
    expect(instret).toBeCloseTo(TIMEBASE_HZ, -4); // one second of guest time
  });

  /**
   * A budget crosses into wasm through `BigInt(...)`, which throws on a fractional
   * value rather than rounding — and a 16ms frame at 10 MHz is 166666.67
   * instructions, so fractions are the *normal* case. An unfloored budget threw
   * inside the animation-frame callback and the guest never booted.
   */
  it("always yields a whole number of instructions", () => {
    const pacer = new Pacer();
    let instret = 0;
    for (let frame = 0; frame < 20; frame++) {
      const budget = pacer.budget(FRAME_MS, instret);
      expect(Number.isInteger(budget)).toBe(true);
      expect(() => BigInt(budget)).not.toThrow();
      instret += budget;
    }
  });

  /** Flooring must not leak: the dropped fractions stay owed, not lost. */
  it("does not drift when fractions are floored away", () => {
    const pacer = new Pacer();
    let instret = 0;
    for (let frame = 0; frame < 600; frame++) {
      instret += pacer.budget(FRAME_MS, instret);
    }
    // Ten seconds of wall clock is 100M guest instructions; a per-frame truncation
    // that leaked would be short by hundreds.
    expect(instret).toBeGreaterThan(TIMEBASE_HZ * 10 - 600);
  });

  it("honours an injected timebase, so the guest's DTB stays the authority", () => {
    const pacer = new Pacer(1_000_000);
    expect(pacer.budget(1000, 0)).toBeCloseTo(500_000, 0); // capped by maxPerFrame
    expect(new Pacer(1_000_000, 10_000_000).budget(1000, 0)).toBeCloseTo(1_000_000, 0);
  });
});
