/**
 * A tour chapter.
 *
 * Routing only, for now: the prose and the guest-at-its-anchor arrive in step 8,
 * when the MDX pipeline and the `tour` crate's manifest cross into the page. What
 * exists here is the shape the shell needs — a landmark and a focusable heading.
 */

import { Link } from "./Link";

export function Chapter({ slug }: { slug: string }) {
  return (
    <main>
      {/*
       * `tabIndex={-1}` so the shell can move focus here on navigation. Not
       * reachable by Tab — this is a target, not a control.
       */}
      <h1 tabIndex={-1}>{slug}</h1>

      {/*
       * The way out. Not navigation — that is chapter two's problem — but a
       * chapter with no exit is a dead end, and without one there is no
       * client-side navigation in the app at all, which leaves the router's
       * back/forward behaviour unreachable by any test.
       */}
      <Link to="/">SnitchOS</Link>
    </main>
  );
}
