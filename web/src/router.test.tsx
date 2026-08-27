import { act, renderHook, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it } from "vitest";
import { navigate, useRoute } from "./router";

beforeEach(() => {
  window.history.replaceState(null, "", "/");
});

describe("useRoute", () => {
  it("reports the route the browser is already at", () => {
    window.history.replaceState(null, "", "/tour/capabilities");

    const { result } = renderHook(() => useRoute());

    expect(result.current).toEqual({ kind: "chapter", slug: "capabilities" });
  });

  it("follows a navigation", () => {
    const { result } = renderHook(() => useRoute());

    act(() => navigate("/tour/capabilities"));

    expect(result.current).toEqual({ kind: "chapter", slug: "capabilities" });
    expect(window.location.pathname).toBe("/tour/capabilities");
  });

  /**
   * **The one hand-rolled SPAs reliably break.**
   *
   * A router that only listens to its own `navigate` looks correct in every test
   * that clicks a link, and strands the reader the moment they press Back — which
   * is the first thing anyone does. `popstate` is the browser telling us it has
   * already moved; the app has to follow, not push.
   */
  it("follows the browser going back", async () => {
    const { result } = renderHook(() => useRoute());

    act(() => navigate("/tour/capabilities"));
    expect(result.current).toEqual({ kind: "chapter", slug: "capabilities" });

    // `back()` is asynchronous, and the `popstate` it fires is the browser's own —
    // dispatching one by hand would test that we listen to ourselves.
    window.history.back();

    await waitFor(() => expect(result.current).toEqual({ kind: "app" }));
  });

  /**
   * Navigating to where you already are should not stack history entries: a reader
   * who clicks the current chapter twice and then presses Back expects to leave,
   * not to arrive at the same page again.
   */
  it("does not stack a history entry for the route it is already on", () => {
    const { result } = renderHook(() => useRoute());
    const depth = window.history.length;

    act(() => navigate("/"));

    expect(result.current).toEqual({ kind: "app" });
    expect(window.history.length).toBe(depth);
  });
});
