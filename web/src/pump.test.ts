import { describe, expect, it, vi } from "vitest";
import type { FrameSource, FrameView, Slice, Status } from "./frames";
import { appendCapped, mips, Pump } from "./pump";

/** A source that returns a scripted sequence of slices — no emulator involved. */
function fakeSource(statuses: Status[]): FrameSource & { calls: number } {
  let i = 0;
  return {
    label: "fake",
    calls: 0,
    advance(_budget: number): Slice {
      this.calls += 1;
      const status = statuses[Math.min(i, statuses.length - 1)] as Status;
      i += 1;
      return { status, text: "", frames: [], instret: i };
    },
  };
}

const view = (kind: string): FrameView => ({ kind, name: null, t: null, value: null });

describe("Pump", () => {
  it("advances the source once per tick", () => {
    const source = fakeSource([{ Running: { instret: 1 } }]);
    const pump = new Pump(source);

    pump.tick(100);
    pump.tick(100);

    expect(source.calls).toBe(2);
  });

  it("passes its budget through to the source", () => {
    const source = fakeSource([{ Running: { instret: 1 } }]);
    const spy = vi.spyOn(source, "advance");

    new Pump(source).tick(12_345);

    expect(spy).toHaveBeenCalledWith(12_345);
  });

  /**
   * The property that keeps a finished guest from burning a core: once halted, the
   * pump must stop *asking*, not merely stop believing the answers. A tab left open
   * on a halted machine would otherwise call into wasm sixty times a second forever.
   */
  it("stops advancing the source once the guest halts", () => {
    const source = fakeSource([{ Running: { instret: 1 } }, { Halted: { instret: 2 } }]);
    const pump = new Pump(source);

    pump.tick(100);
    pump.tick(100);
    const after = source.calls;
    pump.tick(100);
    pump.tick(100);

    expect(source.calls).toBe(after);
    expect(pump.done).toBe(true);
    expect(pump.tick(100)).toBeNull();
  });

  it("stops on a trap too, and the trapping slice is still delivered", () => {
    const source = fakeSource([{ Trapped: { instret: 3, reason: "boom" } }]);
    const pump = new Pump(source);

    const slice = pump.tick(100);

    expect(slice).not.toBeNull();
    expect(slice?.status).toEqual({ Trapped: { instret: 3, reason: "boom" } });
    expect(pump.done).toBe(true);
  });

  /**
   * A zero budget is the steady state once {@link Pacer} has the guest at real time,
   * so it must be free: no wasm call, no slice, no re-render.
   */
  it("does not touch the source when the budget is zero", () => {
    const source = fakeSource([{ Running: { instret: 1 } }]);
    const pump = new Pump(source);

    expect(pump.tick(0)).toBeNull();
    expect(pump.tick(-1)).toBeNull();

    expect(source.calls).toBe(0);
    expect(pump.done).toBe(false);
  });

  /**
   * The pacer reads this every frame to decide what is owed, so it must track what
   * the guest actually retired rather than what was asked for.
   */
  it("reports the guest's own retired count, not the budget asked for", () => {
    const source = fakeSource([{ Running: { instret: 1 } }]);
    const pump = new Pump(source);

    expect(pump.instret).toBe(0);
    const slice = pump.tick(999_999);
    expect(pump.instret).toBe(slice?.instret);
    expect(pump.instret).not.toBe(999_999);
  });

  it("leaves the retired count alone when nothing ran", () => {
    const pump = new Pump(fakeSource([{ Running: { instret: 1 } }]));
    pump.tick(100);
    const before = pump.instret;
    pump.tick(0);
    expect(pump.instret).toBe(before);
  });

  it("is not done before it has run", () => {
    expect(new Pump(fakeSource([{ Running: { instret: 1 } }])).done).toBe(false);
  });
});

describe("appendCapped", () => {
  it("keeps everything while under the cap", () => {
    expect(appendCapped([view("a")], [view("b")], 10)).toHaveLength(2);
  });

  /** The tail is a *tail*: when it overflows, the oldest rows go, not the newest. */
  it("drops the oldest rows once over the cap", () => {
    const existing = [view("a"), view("b"), view("c")];
    const result = appendCapped(existing, [view("d")], 3);

    expect(result.map((f) => f.kind)).toEqual(["b", "c", "d"]);
  });

  it("keeps exactly the cap when the join lands on it", () => {
    expect(appendCapped([view("a")], [view("b")], 2)).toHaveLength(2);
  });

  it("handles an incoming batch larger than the cap on its own", () => {
    const result = appendCapped([view("old")], [view("x"), view("y"), view("z")], 2);
    expect(result.map((f) => f.kind)).toEqual(["y", "z"]);
  });

  /**
   * Identity on an empty batch, so React can skip re-rendering the pane on the many
   * animation frames that carry no telemetry.
   */
  it("returns the same array when nothing is new", () => {
    const existing = [view("a")];
    expect(appendCapped(existing, [], 10)).toBe(existing);
  });
});

describe("mips", () => {
  it("reports millions of instructions per second", () => {
    expect(mips(2_000_000, 1000)).toBeCloseTo(2);
    expect(mips(50_000_000, 2000)).toBeCloseTo(25);
  });

  /** Guards the first frame, where no wall-clock time has elapsed yet. */
  it("is zero rather than infinite before any time has passed", () => {
    expect(mips(1000, 0)).toBe(0);
    expect(mips(1000, -5)).toBe(0);
  });
});
