import { describe, expect, it } from "vitest";
import { encodeInput } from "./input";

describe("encodeInput", () => {
  /**
   * The reason this module exists. A terminal sends CR for Enter; the guest's console
   * wants LF. Untranslated, Enter silently does nothing and the REPL looks broken.
   */
  it("translates a terminal's Enter into the newline the guest expects", () => {
    expect(encodeInput("\r")).toBe("\n");
  });

  it("does not turn a CRLF into two newlines", () => {
    expect(encodeInput("hello\r\n")).toBe("hello\n");
  });

  it("leaves an already-correct newline alone", () => {
    expect(encodeInput("hello\n")).toBe("hello\n");
  });

  /** Tab is the whole point of the completion demo — it must survive untouched. */
  it("passes Tab through", () => {
    expect(encodeInput("let x =\t")).toBe("let x =\t");
  });

  /**
   * Control characters and escape sequences are the terminal's way of saying Ctrl-C
   * and "arrow key". The guest has its own opinions about them and is entitled to
   * them; translating here would be this module overreaching.
   */
  it("passes control characters and escape sequences through", () => {
    expect(encodeInput("\x03")).toBe("\x03");
    expect(encodeInput("\x1b[A")).toBe("\x1b[A");
  });

  it("passes ordinary text through unchanged", () => {
    expect(encodeInput("1.5 + 1.75")).toBe("1.5 + 1.75");
  });

  it("handles an empty chunk", () => {
    expect(encodeInput("")).toBe("");
  });

  /** A paste arrives as one chunk with several lines in it. */
  it("translates every line ending in a pasted block", () => {
    expect(encodeInput("one\rtwo\rthree")).toBe("one\ntwo\nthree");
  });
});
