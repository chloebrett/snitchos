/**
 * Client-side navigation over the History API.
 *
 * Hand-rolled rather than imported, which is the project's habit and here also the
 * point: the emulator must stay alive across navigation, so "navigation" has to
 * mean *not* leaving the document. A real page load would discard the wasm
 * instance and 128 MiB of guest RAM and reboot the machine mid-sentence.
 *
 * Scroll position rides in each history entry's state, because a same-document
 * navigation is not one the browser restores for us.
 */

import { useSyncExternalStore } from "react";
import { type Route, resolve } from "./routes";

/** What we keep per history entry. */
interface EntryState {
  /** Where the reader had scrolled to when they left this entry. */
  scrollY: number;
}

// We restore scroll ourselves, from the offset stored in each history entry. Left
// on "auto", the browser also restores — from a position it recorded for the
// *document*, not for our same-document entries — and the two fight, with the
// browser landing last.
if (typeof window !== "undefined" && "scrollRestoration" in window.history) {
  window.history.scrollRestoration = "manual";
}

const listeners = new Set<() => void>();

function announce(): void {
  for (const listener of listeners) listener();
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  window.addEventListener("popstate", listener);
  return () => {
    listeners.delete(listener);
    window.removeEventListener("popstate", listener);
  };
}

let lastPath: string | null = null;
let lastRoute: Route = { kind: "app" };

/**
 * The current route, as a **stable** value.
 *
 * `useSyncExternalStore` compares snapshots by identity, so resolving afresh on
 * every call reports a change on every render and React gives up with "maximum
 * update depth exceeded". Caching by path is not an optimization here; it is what
 * makes the hook terminate.
 */
function snapshot(): Route {
  const path = window.location.pathname;
  if (path !== lastPath) {
    lastPath = path;
    lastRoute = resolve(path);
  }
  return lastRoute;
}

/**
 * The current route, re-rendering when it changes — whether we moved or the
 * browser did.
 *
 * `useSyncExternalStore` rather than `useState` + an effect: the location is
 * external state that can change before React hears about it, and this is the hook
 * built for exactly that.
 */
export function useRoute(): Route {
  return useSyncExternalStore(subscribe, snapshot);
}

/**
 * Go to `href` without leaving the document.
 *
 * Navigating to the current path is a no-op rather than a new entry: a reader who
 * clicks the chapter they are already reading and then presses Back expects to
 * leave, not to arrive where they already were.
 */
export function navigate(href: string): void {
  if (href === window.location.pathname) return;

  // Remember where they were, so Back can put them back.
  const leaving: EntryState = { scrollY: window.scrollY };
  window.history.replaceState(leaving, "", window.location.pathname);

  window.history.pushState({ scrollY: 0 } satisfies EntryState, "", href);
  announce();
}

/** The scroll offset the entry being restored was left at, if it recorded one. */
export function scrollOf(state: unknown): number {
  if (typeof state !== "object" || state === null) return 0;
  const scrollY = (state as Partial<EntryState>).scrollY;
  return typeof scrollY === "number" ? scrollY : 0;
}
