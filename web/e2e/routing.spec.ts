import { expect, test } from "@playwright/test";

/**
 * Routing, in a real browser.
 *
 * Back/forward and scroll restoration are the two things hand-rolled SPAs
 * reliably break, and neither can be tested in jsdom: `history.back()` there is a
 * queued stub, and there is no layout to scroll. They also happen to be invisible
 * to whoever wrote the router, which is the argument for testing them at all.
 */

test("a chapter has a real, linkable url", async ({ page }) => {
  await page.goto("/tour/capabilities");

  await expect(page.getByRole("heading", { name: /capabilities/i })).toBeVisible();
  expect(new URL(page.url()).pathname).toBe("/tour/capabilities");
});

test("an unknown url says so rather than guessing", async ({ page }) => {
  await page.goto("/nope");

  await expect(page.getByRole("heading", { name: /not found/i })).toBeVisible();
});

/**
 * Back and forward over a *client-side* navigation.
 *
 * Deliberately not two `page.goto`s: those are real page loads, and that version
 * of this test passes with no router at all — the browser would be doing the whole
 * job. The navigation has to come from the app for the test to be about the app.
 */
test("back and forward move between chapter and app", async ({ page }) => {
  await page.goto("/tour/capabilities");

  await page.evaluate(() => {
    document.querySelector<HTMLAnchorElement>('a[href="/"]')?.click();
  });
  await expect.poll(() => new URL(page.url()).pathname).toBe("/");

  await page.goBack();
  await expect.poll(() => new URL(page.url()).pathname).toBe("/tour/capabilities");
  await expect(page.getByRole("heading", { name: /capabilities/i })).toBeVisible();

  await page.goForward();
  await expect.poll(() => new URL(page.url()).pathname).toBe("/");
});

/**
 * **The reader's place in the page survives leaving and coming back.**
 *
 * A browser restores scroll for a real navigation. A same-document one it does
 * not, so the offset has to ride in the history entry — and the failure is
 * peculiarly annoying: you read half a chapter, follow a link, press Back, and
 * are returned to the top with no indication anything was lost.
 */
test("going back restores where the reader had scrolled to", async ({ page }) => {
  await page.goto("/tour/capabilities");

  // Chapters have no prose yet (step 8), so give the page something to scroll
  // through. It is appended outside the React root and the navigation below is
  // same-document, so it survives — which is the property under test.
  await page.evaluate(() => {
    const filler = document.createElement("div");
    filler.style.height = "4000px";
    document.body.append(filler);
    window.scrollTo(0, 1200);
  });
  await expect.poll(() => page.evaluate(() => window.scrollY)).toBeGreaterThan(1000);

  // A *client-side* navigation. Two things this deliberately is not:
  //   - `page.goto`, which is a real page load and tests nothing about this router;
  //   - `locator.click()`, which scrolls the link into view first — the link is at
  //     the top, so Playwright would helpfully undo the scroll we are about to
  //     assert was remembered. (Measured: it recorded `scrollY: 0`.)
  await page.evaluate(() => {
    document.querySelector<HTMLAnchorElement>('a[href="/"]')?.click();
  });
  await expect.poll(() => new URL(page.url()).pathname).toBe("/");

  await page.goBack();

  await expect.poll(() => new URL(page.url()).pathname).toBe("/tour/capabilities");
  await expect.poll(() => page.evaluate(() => window.scrollY)).toBeGreaterThan(1000);
});
