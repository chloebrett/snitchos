import { expect, test } from "@playwright/test";

/**
 * The panels, against a real guest.
 *
 * Everything below the UI is unit-tested — the folds in `diagram`, the retention
 * policy in `snemu-wasm`, the layout and components in Vitest. What none of those can
 * establish is that the whole chain produces a *true* view of a machine that is
 * actually running: decode → retain → fold → serialize → render.
 */

const BOOT_TIMEOUT = 120_000;

test("shows the capability derivation tree of a running guest", async ({ page }) => {
  await page.goto("/");

  // `init` is the default boot and the reason the cap-id spine exists: it creates an
  // endpoint, spawns a filesystem server delegating RECV|MINT, and spawns a client
  // holding a minted SEND. That is a derivation tree with real edges in it.
  await expect(page.getByTestId("graph")).toBeVisible({ timeout: BOOT_TIMEOUT });

  // Named, not numbered. A tree of `h1 → h2` would mean the registrations that give
  // holders their names were dropped — the exact failure the retention split exists
  // to prevent, and one that looks plausible rather than broken.
  await expect(page.getByTestId("graph")).toContainText(/init|fs|kvetch|stitch/, {
    timeout: BOOT_TIMEOUT,
  });
});

test("folds spans and switches too", async ({ page }) => {
  await page.goto("/");
  await expect(page.getByTestId("graph")).toBeVisible({ timeout: BOOT_TIMEOUT });

  await page.getByTestId("tab-spans").click();
  await expect(page.getByTestId("graph")).toContainText("kernel", {
    timeout: BOOT_TIMEOUT,
  });

  await page.getByTestId("tab-switches").click();
  // A switch graph needs at least two tasks to have transitioned between.
  await expect(page.getByTestId("graph-node").first()).toBeVisible({
    timeout: BOOT_TIMEOUT,
  });
});

test("the raw frame tail is still reachable", async ({ page }) => {
  await page.goto("/");
  await page.getByTestId("tab-frames").click();

  // Something the guest emits continuously: the tail is a bounded window, so a
  // boot-time frame is long gone by the time anything looks for it.
  await expect(page.getByTestId("frame-list")).toContainText("snitchos.", {
    timeout: BOOT_TIMEOUT,
  });
});

test("switching workload clears the panels", async ({ page }) => {
  await page.goto("/");
  await expect(page.getByTestId("graph")).toBeVisible({ timeout: BOOT_TIMEOUT });

  await page.getByTestId("workload").selectOption("stitch-repl");

  // Stale structure from the previous guest would be worse than none: it would
  // attribute one machine's capabilities to another.
  await expect(page.getByText(/waiting for the guest/)).toBeVisible({ timeout: 10_000 });
});

test("the retained-frame count stays bounded on a long run", async ({ page }) => {
  await page.goto("/");
  await expect(page.getByTestId("durable-count")).toBeVisible({ timeout: BOOT_TIMEOUT });

  const read = async () =>
    Number.parseInt((await page.getByTestId("durable-count").textContent()) ?? "0", 10);

  const early = await read();
  await page.waitForTimeout(6000);
  const later = await read();

  // The durable bucket has no ceiling by design, on the assumption that registrations
  // and capability lifecycle events are naturally few. This is that assumption under
  // measurement rather than under trust — the same assumption class that was already
  // wrong once here, about the guest's idle task.
  expect(later - early).toBeLessThan(500);
});
