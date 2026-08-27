import { act, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it } from "vitest";
import { navigate } from "./router";
import { Shell } from "./Shell";

beforeEach(() => {
  window.history.replaceState(null, "", "/");
});

/** A stand-in for the emulator page, which must not boot in jsdom. */
const app = <h1 tabIndex={-1}>SnitchOS</h1>;

describe("Shell", () => {
  it("shows the emulator at the root", () => {
    render(<Shell app={app} />);

    expect(screen.getByRole("heading", { name: "SnitchOS" })).toBeInTheDocument();
  });

  it("shows a chapter at its own url", () => {
    window.history.replaceState(null, "", "/tour/capabilities");

    render(<Shell app={app} />);

    expect(screen.getByRole("heading", { name: /capabilities/i })).toBeInTheDocument();
  });

  it("says so plainly when a url names nothing", () => {
    window.history.replaceState(null, "", "/nope");

    render(<Shell app={app} />);

    expect(screen.getByRole("heading", { name: /not found/i })).toBeInTheDocument();
  });

  /**
   * **A client-side navigation is invisible to a screen reader.**
   *
   * The browser announces a real page load and moves focus to the top of the new
   * document. `pushState` does neither: focus stays on the link that was clicked —
   * which no longer exists — and nothing is announced, so a reader using a screen
   * reader is told nothing happened. Moving focus to the new heading is the
   * standard repair, and it is invisible to the person who wrote the router, which
   * is why it needs a test rather than a memory.
   */
  it("moves focus to the new heading when the route changes", async () => {
    render(<Shell app={app} />);

    act(() => navigate("/tour/capabilities"));

    await waitFor(() => {
      const heading = screen.getByRole("heading", { name: /capabilities/i });
      expect(document.activeElement).toBe(heading);
    });
  });

  /**
   * Focus alone moves the cursor but says nothing about *what* happened. A polite
   * live region is what turns the move into "Who is allowed to do what".
   */
  it("announces the new page in a live region", async () => {
    render(<Shell app={app} />);

    act(() => navigate("/tour/capabilities"));

    await waitFor(() => {
      expect(screen.getByRole("status")).toHaveTextContent(/capabilities/i);
    });
  });

  /**
   * **But not on arrival.** Stealing focus on first paint fights the browser, which
   * has already put the reader at the top of a freshly loaded document, and it
   * skips past anything above the heading. Only a *change* of route is a thing to
   * announce.
   */
  it("does not steal focus on first render", () => {
    render(<Shell app={app} />);

    expect(document.activeElement).toBe(document.body);
    expect(screen.getByRole("status")).toHaveTextContent("");
  });
});
