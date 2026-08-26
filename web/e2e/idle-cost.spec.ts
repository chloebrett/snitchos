import { expect, test } from "@playwright/test";

/**
 * What does this page cost while it is just sitting there?
 *
 * `docs/scaling-down-snitchos.md` predicted "an idle tab would pin a core". It was
 * right, and measuring it is what turned a milestone-4 nicety into a prerequisite —
 * and then what proved the fix. The diagnosis was not what the prediction implied,
 * though: nothing was being wasted. snemu's clock *is* the retired-instruction
 * count, so a second of guest time costs the timebase in instructions, and a loop
 * with no pacing simply buys guest time as fast as the host can make it.
 *
 * `Performance.getMetrics` over CDP gives `TaskDuration` — cumulative main-thread
 * busy seconds — so the busy *fraction* over a window is a real number rather than a
 * proxy for one.
 */

const BOOT_TIMEOUT = 90_000;
const WINDOW_MS = 4000;

test("reports what an open tab costs once the guest has booted @measurement", async ({
  page,
}) => {
  await page.goto("/");
  await expect(page.getByTestId("console")).toContainText(/heartbeat/i, {
    timeout: BOOT_TIMEOUT,
  });

  const client = await page.context().newCDPSession(page);
  await client.send("Performance.enable");

  const taskSeconds = async (): Promise<number> => {
    const { metrics } = await client.send("Performance.getMetrics");
    return metrics.find((m) => m.name === "TaskDuration")?.value ?? 0;
  };
  const instret = async (): Promise<number> => {
    const text = (await page.getByTestId("instret").textContent()) ?? "0M";
    return Number.parseFloat(text) * 1e6;
  };

  const busyBefore = await taskSeconds();
  const instretBefore = await instret();
  const wallBefore = Date.now();

  await page.waitForTimeout(WINDOW_MS);

  const busyDelta = (await taskSeconds()) - busyBefore;
  const instretDelta = (await instret()) - instretBefore;
  const wallDelta = (Date.now() - wallBefore) / 1000;

  const busyFraction = busyDelta / wallDelta;
  const guestMips = instretDelta / 1e6 / wallDelta;

  console.log(
    `\nidle cost over ${wallDelta.toFixed(1)}s post-boot:\n` +
      `  main thread busy : ${(busyFraction * 100).toFixed(1)}% of one core\n` +
      `  guest throughput : ${guestMips.toFixed(1)} MIPS\n`,
  );

  // The history this number carries, because it is the point of keeping it:
  //
  //   100.0% of a core, 11.0 MIPS — the default interpreter, running flat out.
  //    100.0% of a core, 38.9 MIPS — snemu's caches enabled. Faster, not cheaper:
  //                                  the loop simply bought guest time quicker.
  //     43.3% of a core, 10.0 MIPS — wall-clock pacing. 10.0 is the guest timebase
  //                                  exactly, so the guest now runs at real time.
  //
  // The bar is one third of a core with headroom. The remaining cost is not the
  // emulator — at 10 MIPS of a 38.9 MIPS ceiling that is roughly a quarter of a core
  // — so the rest is the page: React re-renders and xterm writes, every frame. That
  // is the next thing to measure if this needs to come down further.
  //
  // Tagged `@measurement` and excluded from the default e2e run (see
  // playwright.config.ts), because a number worth watching is not the same as a
  // regression gate; run it with `yarn measure`.
  expect(busyFraction).toBeLessThan(0.6);
});
