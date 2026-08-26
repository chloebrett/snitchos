import { expect, test } from "@playwright/test";

/**
 * The milestone's headline claim: a machine in a tab that you can pick, type at, and
 * get answers from.
 *
 * Only checkable here. The Rust suite has no browser, `wasm-pack test --node` has no
 * DOM or keyboard, and the unit suite's fakes cannot boot a kernel.
 */

/** Booting a REPL and reaching its prompt is real work; be generous on slow machines. */
const BOOT_TIMEOUT = 120_000;

/** Select a workload and wait for the guest to reboot into it. */
async function boot(page: import("@playwright/test").Page, workload: string) {
  await page.goto("/");
  await page.getByTestId("workload").selectOption(workload);
  // The terminal is cleared on switch, so anything appearing after this is the new
  // guest — no risk of matching the previous boot's output.
  await expect(page.getByTestId("instret")).not.toHaveText("0.0M", {
    timeout: BOOT_TIMEOUT,
  });
}

test("offers the curated workloads, default first", async ({ page }) => {
  await page.goto("/");
  const options = page.getByTestId("workload").locator("option");

  await expect(options.first()).toHaveText(/init/i);
  await expect(options.filter({ hasText: /Stitch REPL$/ })).toHaveCount(1);
  await expect(options.filter({ hasText: /trained model/i })).toHaveCount(1);
});

test("boots the Stitch REPL to a prompt", async ({ page }) => {
  await boot(page, "stitch-repl");
  await expect(page.getByTestId("console")).toContainText("stitch>", {
    timeout: BOOT_TIMEOUT,
  });
});

test("evaluates what you type", async ({ page }) => {
  await boot(page, "stitch-repl");
  await expect(page.getByTestId("console")).toContainText("stitch>", {
    timeout: BOOT_TIMEOUT,
  });

  // Typed through the real keyboard path: xterm's `onData`, the CR→LF translation,
  // `push_input`, the guest's UART. Pressing Enter is what needed that translation —
  // untranslated it does nothing at all and the REPL looks broken.
  await page.getByTestId("console").click();
  await page.keyboard.type("1.5 + 1.75");
  await page.keyboard.press("Enter");

  await expect(page.getByTestId("console")).toContainText("3.25", {
    timeout: BOOT_TIMEOUT,
  });
});

test("answers Tab with a completion from the trained model", async ({ page }) => {
  // The showcase, and the slow one: ~417M guest instructions, so turbo rather than
  // real time. See the plan's step 0 — paced this is ~42s of waiting.
  await page.goto("/");
  await page.getByTestId("turbo").check();
  await page.getByTestId("workload").selectOption("stitch-drivel");

  await expect(page.getByTestId("console")).toContainText("stitch>", {
    timeout: BOOT_TIMEOUT,
  });

  await page.getByTestId("console").click();
  await page.keyboard.type("let x =");
  await page.keyboard.press("Tab");

  // The model working is observable while you wait — which is the point of showing
  // it in this project rather than apologising for the delay.
  await expect(page.getByTestId("frame-list")).toContainText("kvetch.complete", {
    timeout: BOOT_TIMEOUT,
  });

  // And the suggestion has to reach the *terminal*, not merely the wire: the client
  // re-validates it and falls back to a grammar menu if it refuses, which looks
  // identical from the frame stream.
  await expect(page.getByTestId("console")).toContainText(/stitch> let x =\s*\S/, {
    timeout: BOOT_TIMEOUT,
  });
});
