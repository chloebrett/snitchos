/**
 * The app shell: one document, many routes, one guest.
 *
 * The emulator is passed in rather than constructed here so it lives *above* the
 * route switch — a chapter changing must not unmount the machine, which is the
 * whole reason this is a single-page app and not a docs site.
 */

import { type ReactNode, useEffect, useRef, useState } from "react";
import { Chapter } from "./Chapter";
import { scrollOf, useRoute } from "./router";
import type { Route } from "./routes";

/** What a route calls itself, for the announcement. */
function titleOf(route: Route): string {
  switch (route.kind) {
    case "app":
      return "SnitchOS";
    case "chapter":
      return route.slug;
    case "notFound":
      return "Not found";
  }
}

export function Shell({ app }: { app: ReactNode }) {
  const route = useRoute();
  const [announcement, setAnnouncement] = useState("");

  // The first render is an arrival, not a navigation: the browser has already put
  // the reader at the top of the document. Only a *change* is worth announcing.
  const arrived = useRef(false);
  const heading = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    if (!arrived.current) {
      arrived.current = true;
      return;
    }

    heading.current?.querySelector("h1")?.focus();
    setAnnouncement(titleOf(route));

    // A same-document navigation is not one the browser restores scroll for, so the
    // offset rides in the history entry (see `router.ts`).
    //
    // After a frame, not immediately: this effect runs before the new route's
    // content has been laid out, and scrolling to 1200px in a document that is
    // still 0px tall silently lands at 0.
    const target = scrollOf(window.history.state);
    const frame = requestAnimationFrame(() => window.scrollTo(0, target));
    return () => cancelAnimationFrame(frame);
  }, [route]);

  return (
    <>
      {/*
       * Politely, and always present: a live region added to the DOM at the moment
       * it has something to say is frequently not announced at all, because the
       * screen reader was not watching it yet.
       */}
      <div role="status" aria-live="polite" className="sr-only">
        {announcement}
      </div>

      {/*
       * `display: contents` so this wrapper is not a box. It exists only to give
       * the focus effect somewhere to look; as a real element it broke the
       * emulator page's height chain from `#root`, and xterm's fit addon then
       * resized forever — the terminal never became stable enough to click.
       */}
      <div ref={heading} style={{ display: "contents" }}>
        {route.kind === "app" && app}
        {route.kind === "chapter" && <Chapter slug={route.slug} />}
        {route.kind === "notFound" && (
          <main>
            <h1 tabIndex={-1}>Not found</h1>
            <p>
              Nothing lives at <code>{route.path}</code>.
            </p>
          </main>
        )}
      </div>
    </>
  );
}
