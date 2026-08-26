import { describe, expect, it } from "vitest";
import { progressLabel, readWithProgress } from "./progress";

describe("progressLabel", () => {
  it("reports how much of the whole has arrived", () => {
    expect(progressLabel(2_100_000, 6_400_000)).toBe(
      "loading kernel — 2.1 MB of 6.4 MB (33%)",
    );
  });

  /**
   * A `Content-Length` is not guaranteed. Without one there is no denominator, and
   * inventing one would mean inventing a percentage.
   */
  it("reports bytes alone when the server did not say how many to expect", () => {
    expect(progressLabel(2_100_000, null)).toBe("loading kernel — 2.1 MB");
    expect(progressLabel(2_100_000, 0)).toBe("loading kernel — 2.1 MB");
  });

  /**
   * The trap worth having a test for: a gzipped response reports the **compressed**
   * length while the stream yields **decompressed** bytes, so `received` genuinely
   * overshoots `total`. Unclamped, the page counts confidently past 100%.
   */
  it("clamps at 100% when a compressed response overshoots its content-length", () => {
    expect(progressLabel(6_400_000, 2_000_000)).toContain("(100%)");
  });

  it("starts at zero rather than at nothing", () => {
    expect(progressLabel(0, 6_400_000)).toBe("loading kernel — 0.0 MB of 6.4 MB (0%)");
  });
});

describe("readWithProgress", () => {
  /** Build a Response whose body arrives in several chunks. */
  function chunked(chunks: Uint8Array[], contentLength?: string): Response {
    const stream = new ReadableStream<Uint8Array>({
      start(controller) {
        for (const c of chunks) controller.enqueue(c);
        controller.close();
      },
    });
    const headers = new Headers(contentLength ? { "content-length": contentLength } : {});
    return new Response(stream, { headers });
  }

  it("reassembles the chunks in order", async () => {
    const body = await readWithProgress(
      chunked([new Uint8Array([1, 2]), new Uint8Array([3]), new Uint8Array([4, 5])]),
      () => {},
    );
    expect(Array.from(body)).toEqual([1, 2, 3, 4, 5]);
  });

  /** The point of streaming: something to say *before* the download finishes. */
  it("reports progress as each chunk lands, not once at the end", async () => {
    const seen: Array<[number, number | null]> = [];
    await readWithProgress(
      chunked([new Uint8Array(10), new Uint8Array(20), new Uint8Array(30)], "60"),
      (received, total) => seen.push([received, total]),
    );

    expect(seen).toEqual([
      [10, 60],
      [30, 60],
      [60, 60],
    ]);
  });

  it("passes on a missing content-length rather than guessing one", async () => {
    const seen: Array<number | null> = [];
    await readWithProgress(chunked([new Uint8Array(4)]), (_r, total) => seen.push(total));
    expect(seen).toEqual([null]);
  });

  it("handles an empty body", async () => {
    const body = await readWithProgress(chunked([]), () => {});
    expect(body.length).toBe(0);
  });
});
