import { expect, test } from "@playwright/test";

/**
 * The metric charts, against a real guest.
 *
 * Everything under them is unit-tested — retention in Rust, scales and grouping and
 * rate derivation in Vitest. What none of that establishes is that the chain produces
 * a *chart of a machine that is actually running*: decode → per-metric retention →
 * serialize → group → scale → path.
 */

const BOOT_TIMEOUT = 120_000;

test("plots the guest's own metrics", async ({ page }) => {
  await page.goto("/");
  await page.getByTestId("tab-metrics").click();

  // The guest emits ~60 metrics every heartbeat, so groups appear as soon as it is
  // running. `heap` and `sched` are the ones a reader would look for first.
  await expect(page.getByTestId("metric-groups")).toContainText(/heap|sched/, {
    timeout: BOOT_TIMEOUT,
  });

  // A drawn line, not merely an axis frame: two samples are needed before a path
  // exists at all, so this also proves samples are accumulating over time rather
  // than each fold replacing the last.
  await expect(page.locator("[data-testid=metric-charts] path[d]").first()).toHaveAttribute(
    "d",
    /^M.*L/,
    { timeout: BOOT_TIMEOUT },
  );
});

test("groups are selectable", async ({ page }) => {
  await page.goto("/");
  await page.getByTestId("tab-metrics").click();
  await expect(page.getByTestId("group-sched")).toBeVisible({ timeout: BOOT_TIMEOUT });

  await page.getByTestId("group-sched").click();
  await expect(page.getByTestId("group-sched")).toHaveAttribute("aria-pressed", "true");
  await expect(page.getByTestId("metric-charts")).toContainText("context_switches_total");
});

test("a counter is charted as a rate, and says it is derived", async ({ page }) => {
  await page.goto("/");
  await page.getByTestId("tab-metrics").click();
  await expect(page.getByTestId("group-sched")).toBeVisible({ timeout: BOOT_TIMEOUT });
  await page.getByTestId("group-sched").click();

  // The guest never emitted a rate. Saying so on the axis is the difference between
  // a computed view and a claim about what was measured.
  await expect(page.getByTestId("metric-charts")).toContainText("derived", {
    timeout: BOOT_TIMEOUT,
  });
});

test("switching workload clears the charts", async ({ page }) => {
  await page.goto("/");
  await page.getByTestId("tab-metrics").click();
  await expect(page.getByTestId("metric-groups")).toBeVisible({ timeout: BOOT_TIMEOUT });

  await page.getByTestId("workload").selectOption("stitch-repl");

  // Carrying one machine's numbers into another's chart would be worse than showing
  // none — it would attribute measurements to a guest that never made them.
  await expect(page.getByText(/waiting for the guest/)).toBeVisible({ timeout: 10_000 });
});
