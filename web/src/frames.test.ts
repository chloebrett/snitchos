import { describe as describeStatus, isTerminal, type Status } from "./frames";
import { describe, expect, it } from "vitest";

describe("status", () => {
  it("keeps scheduling while the guest is running", () => {
    expect(isTerminal({ Running: { instret: 10 } })).toBe(false);
  });

  /**
   * The two terminal cases exist precisely so the loop stops. A `Halted` that read
   * as non-terminal would schedule animation frames forever over a guest that can
   * never move again.
   */
  it("stops on halt and on trap", () => {
    expect(isTerminal({ Halted: { instret: 10 } })).toBe(true);
    expect(isTerminal({ Trapped: { instret: 10, reason: "boom" } })).toBe(true);
  });

  it("describes each outcome for the status line", () => {
    expect(describeStatus({ Running: { instret: 1 } })).toBe("running");
    expect(describeStatus({ Halted: { instret: 1 } })).toContain("halted");
  });

  /** A trap's reason is the whole value of reporting it — don't drop it. */
  it("carries a trap's reason through to the description", () => {
    const status: Status = {
      Trapped: { instret: 1, reason: "Unimplemented { pc: 0x80000000, instr: 0 }" },
    };
    expect(describeStatus(status)).toContain("Unimplemented");
  });
});
