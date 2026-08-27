/**
 * What a URL means.
 *
 * Real paths, not hashes: a chapter has to be linkable, and `#/tour/x` is a URL a
 * reader cannot send to anyone who expects it to look like a page. The cost is
 * that the dev server and any host must rewrite unknown paths to `index.html`.
 *
 * Reading and writing URLs both live here. Hand-written `href`s are how a route
 * and its links drift apart, and the failure is a 404 nobody sees until a reader
 * clicks.
 */

/** Where the reader is. */
export type Route =
  | { kind: "app" }
  | { kind: "chapter"; slug: string }
  | { kind: "notFound"; path: string };

/** The path prefix chapters live under. */
const TOUR = "tour";

/** The route a pathname names. */
export function resolve(pathname: string): Route {
  // Destructured rather than indexed: under `noUncheckedIndexedAccess` a length
  // check does not narrow `segments[1]`, and the honest fix is to handle the
  // absence rather than assert it away.
  const [section, slug, ...rest] = pathname.split("/").filter((s) => s.length > 0);

  if (section === undefined) return { kind: "app" };
  if (section === TOUR && slug !== undefined && rest.length === 0) {
    return { kind: "chapter", slug };
  }
  return { kind: "notFound", path: pathname };
}

/** The path that resolves back to `route`. */
export function hrefFor(route: Route): string {
  switch (route.kind) {
    case "app":
      return "/";
    case "chapter":
      return `/${TOUR}/${route.slug}`;
    case "notFound":
      return route.path;
  }
}
