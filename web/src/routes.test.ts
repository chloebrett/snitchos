import { describe, expect, it } from "vitest";
import { hrefFor, resolve } from "./routes";

describe("resolve", () => {
  it("serves the emulator at the root", () => {
    expect(resolve("/")).toEqual({ kind: "app" });
  });

  it("serves a chapter by slug", () => {
    expect(resolve("/tour/capabilities")).toEqual({
      kind: "chapter",
      slug: "capabilities",
    });
  });

  /**
   * A trailing slash is the same page, not a different one. Two URLs for one
   * chapter would split anything keyed on the path — scroll positions, an
   * eventual analytics count, a reader's own bookmark.
   */
  it("treats a trailing slash as the same chapter", () => {
    expect(resolve("/tour/capabilities/")).toEqual({
      kind: "chapter",
      slug: "capabilities",
    });
  });

  /**
   * The chapter index does not exist yet — there is one chapter, so a list of it
   * would be a page saying "capabilities". Until it does, `/tour` is not found
   * rather than silently the app: a link that goes somewhere unintended is worse
   * than one that admits it is broken.
   */
  it("has no chapter index yet", () => {
    expect(resolve("/tour")).toEqual({ kind: "notFound", path: "/tour" });
  });

  it("reports an unknown path rather than guessing", () => {
    expect(resolve("/nope")).toEqual({ kind: "notFound", path: "/nope" });
  });

  /**
   * A slug is one path segment. `/tour/a/b` naming chapter `a` would make two
   * distinct URLs resolve to one chapter, which is the same split as the trailing
   * slash and harder to notice.
   */
  it("does not read a nested path as a chapter", () => {
    expect(resolve("/tour/a/b")).toEqual({ kind: "notFound", path: "/tour/a/b" });
  });
});

describe("hrefFor", () => {
  /**
   * Links are built from the same table that reads them. Hand-written `href`s are
   * how a route and its links drift apart, and the failure is a 404 that only
   * appears when someone clicks.
   */
  it("builds the href a chapter route resolves back from", () => {
    const href = hrefFor({ kind: "chapter", slug: "capabilities" });

    expect(href).toBe("/tour/capabilities");
    expect(resolve(href)).toEqual({ kind: "chapter", slug: "capabilities" });
  });

  it("builds the root href", () => {
    expect(hrefFor({ kind: "app" })).toBe("/");
  });
});
