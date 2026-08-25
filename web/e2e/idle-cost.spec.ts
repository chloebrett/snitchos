import { expect, test } from "@playwright/test";

/**
 * What does this page cost while it is just sitting there?
 *
 * snemu's clock is the instruction counter, and it fast-forwards over an idle `wfi`
 * rather than sleeping — `docs/scaling-down-snitchos.md` flags that "an idle tab
 * would pin a core" as a known gap. That was a prediction, not a measurement. This
 * measures it, because the difference decides whether wall-clock pacing is a
 * milestone-4 nicety or a prerequisite for showing anyone the page.
 *
 * `Performance.getMetrics` over CDP gives `TaskDuration` — cumulative main-thread
 * busy seconds — so the busy *fraction* over a window is a real number rather than a
 * proxy for one.
 */

const BOOT_TIMEOUT = 90_000;
const WINDOW_MS = 4000;

test("reports what an open tab costs once the guest has booted @measurement", async ({ page }) => {
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

  // MEASURED 2026-08-25: 100.0% of one core, sustained, at 11 MIPS of guest
  // throughput. The prediction in scaling-down-snitchos.md was right and not
  // marginal — this tab pins a core for as long as it is open.
  //
  // The assertion is deliberately left FAILING rather than relaxed to match. A
  // threshold moved to accommodate the bug would turn the only evidence of it into a
  // green tick; a red test is the honest record until wall-clock pacing lands, and
  // it is what will confirm the fix.
  //
  // Tagged `@measurement` and excluded from the default e2e run (see
  // playwright.config.ts) so it does not mask real regressions; run it with
  //   yarn measure
  expect(busyFraction).toBeLessThan(0.9);
});
