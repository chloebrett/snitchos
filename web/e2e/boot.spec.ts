import { expect, test } from "@playwright/test";

/**
 * The milestone's acceptance criteria, in the only place they are checkable.
 *
 * None of these are reachable from `cargo nextest` (no browser) or from
 * `wasm-pack test --node` (no DOM, no animation frames). They are claims about a
 * real browser running a real kernel, and until now the plan recorded them as
 * "drive the page and observe".
 *
 * Requires `cargo xtask web` to have staged `public/kernel.elf` first; without it the
 * app reports a missing kernel and these fail with that message, which is the correct
 * and legible failure.
 */

/** Boot to heartbeat is ~25M guest instructions; be generous on slower machines. */
const BOOT_TIMEOUT = 90_000;

test("boots the kernel and reaches its heartbeat", async ({ page }) => {
  await page.goto("/");

  // The kernel's own boot log, in a real terminal emulator. `kernel.heartbeat` is
  // the milestone: it means the scheduler is running and the timer is firing.
  await expect(page.getByTestId("console")).toContainText(/heartbeat/i, {
    timeout: BOOT_TIMEOUT,
  });
});

test("decodes telemetry and resolves interned names", async ({ page }) => {
  await page.goto("/");

  // `kernel.boot` is the first interned string on the wire. Seeing it as text rather
  // than a numeric StringId proves the whole chain: virtio drain → COBS/postcard
  // decode → intern table → projection → render.
  await expect(page.getByTestId("frame-list")).toContainText("kernel.boot", {
    timeout: BOOT_TIMEOUT,
  });
});

test("stays responsive while the guest boots", async ({ page }) => {
  await page.goto("/");

  // Wait until the guest is definitely working, so responsiveness is measured
  // *during* emulation rather than before it starts.
  await expect(page.getByTestId("instret")).not.toHaveText("0.0M", {
    timeout: BOOT_TIMEOUT,
  });

  // If the rAF loop ever ran to completion inside one frame, the main thread would
  // be blocked and neither of these could happen: the pulse animation would freeze
  // and a round-trip evaluation would stall behind it.
  //
  // The bar is 20fps over half a second. It started at ">2 frames", which sounds
  // strict and is not — that passes at 5.5fps, which is exactly the jank the
  // unbounded 2M-instret slice produced (~180ms per frame at the then-current
  // 11 MIPS). A threshold that a known-bad build satisfies is not a test. Pacing
  // holds ~60fps here, so 20 leaves room for a slow CI box while still failing
  // anything a person would call stuttering.
  const framesPainted = await countAnimationFrames(page);
  expect(framesPainted).toBeGreaterThan(10);

  // And the page still answers a fresh evaluation promptly.
  const started = Date.now();
  await page.evaluate(() => document.title);
  expect(Date.now() - started).toBeLessThan(2000);
});

test("two loads of the same kernel produce byte-identical output", async ({ page }) => {
  // Determinism is snemu's headline property — its clock is the instruction counter,
  // not wall time — and this is the assertion that it survives the browser. If this
  // ever fails, something has introduced a source of entropy into the guest.
  const first = await bootAndCapture(page);
  const second = await bootAndCapture(page);

  expect(second).toBe(first);
  expect(first.length).toBeGreaterThan(0);
});

/** How many animation frames the page paints in ~500ms. */
async function countAnimationFrames(
  page: import("@playwright/test").Page,
): Promise<number> {
  return page.evaluate(
    () =>
      new Promise<number>((resolve) => {
        let frames = 0;
        const stop = performance.now() + 500;
        const tick = () => {
          frames += 1;
          if (performance.now() < stop) requestAnimationFrame(tick);
          else resolve(frames);
        };
        requestAnimationFrame(tick);
      }),
  );
}

/**
 * Load the page, run to a fixed instret milestone, and return the console text.
 *
 * Stopping at a fixed *guest* milestone rather than after a fixed wall-clock time is
 * what makes the comparison meaningful: two runs that executed the same number of
 * instructions must have produced the same bytes, however fast the host was.
 */
async function bootAndCapture(page: import("@playwright/test").Page): Promise<string> {
  await page.goto("/", { waitUntil: "load" });
  await expect(page.getByTestId("console")).toContainText(/heartbeat/i, {
    timeout: BOOT_TIMEOUT,
  });
  return (await page.getByTestId("console").textContent()) ?? "";
}
