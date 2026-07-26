// semver.st — semantic-version parsing, comparison, and sorting.
// `Version`'s prerelease field is the batch's first real `Either` usage:
// `Left(tag)` for a prerelease build, `Right(())` for a plain release —
// genuinely neither side is "the error," which is exactly what `Either`
// (as opposed to `Result`) is for.
//
// **`Either` has to be declared here, not used as a builtin.** Unlike
// `Maybe`/`Result` (registered natively — see `registry.rs::register_builtin_types`,
// `Some`/`Ok`/`Err` are hardcoded constructors), `Either`/`Left`/`Right`
// are *nowhere* in `registry.rs`, `interp.rs`, or `check.rs` — despite
// being documented in `docs/language-design.md` right alongside `Maybe`/
// `Result` as one of the "just sums" standard types. It's declared exactly
// like any other user `sum` below; that works identically to a builtin
// (sums are structurally the same regardless of where they're declared),
// it just isn't ambient the way `Maybe`/`Result` are — the same "documented
// but never actually built" gap `Set` turned out to be (spellcheck.st's
// entry), one step short of it (this one is at least easy to supply
// yourself in three words).

use Str
use List

sum Either<A, B> = Left(A) | Right(B)

prod Version(major: Int, minor: Int, patch: Int, pre: Either<Str, ()>)

findSubstr(s: Str, sub: Str, from: Int) -> Maybe<Int> =
    from + Str.length(sub) > Str.length(s)
        => None
        | (Str.slice(s, from, from + Str.length(sub)) == sub => Some(from) | findSubstr(s, sub, from + 1))

partAt(parts: List<Str>, i: Int) -> Str = List.at(parts, i) |> unwrapOr("")

parseVersion(s: Str) -> Result<Version, Str> = {
    let dash = findSubstr(s, "-", 0)
    let core = match dash { Some(i) => Str.slice(s, 0, i)  None => s }
    let pre = match dash { Some(i) => Left(Str.slice(s, i + 1, Str.length(s)))  None => Right(()) }
    let parts = Str.split(core, ".")
    count(parts) != 3
        => Err("expected major.minor.patch, got '" + s + "'")
        | match (Str.parseInt(partAt(parts, 0)), Str.parseInt(partAt(parts, 1)), Str.parseInt(partAt(parts, 2))) {
            (Some(ma), Some(mi), Some(pa)) => Ok(Version(ma, mi, pa, pre))
            _ => Err("version components must be integers, got '" + s + "'")
        }
}

showVersion(v: Version) -> Str =
    toStr(v.major) + "." + toStr(v.minor) + "." + toStr(v.patch)
        + match v.pre { Left(tag) => "-" + tag  Right(_) => "" }

// Negative/zero/positive like a C-style `compare`, checked field by field
// (major, then minor, then patch, then prerelease) — the first field that
// differs decides the whole comparison.
compareVersions(a: Version, b: Version) -> Int =
    a.major != b.major
        => a.major - b.major
        | (a.minor != b.minor
            => a.minor - b.minor
            | (a.patch != b.patch
                => a.patch - b.patch
                | comparePre(a.pre, b.pre)))

// A release outranks a prerelease at the same major.minor.patch (`1.0.0` >
// `1.0.0-alpha`) — the one asymmetric rule semver actually specifies here;
// two prereleases compare lexicographically by tag, which isn't part of
// the spec (real semver's prerelease comparison is dot-separated-identifier
// aware) but is a reasonable, simple stand-in for a file whose point is
// demonstrating `Either`, not implementing the whole spec.
// Nested matches on `a` then `b`, not one `match (a, b) { (Right(_), …) => … }`
// — a tuple-pattern match arm whose body is a bare value immediately
// followed by the next arm's tuple pattern hits the maximal-munch call-paren
// gotcha *again* (plans/stitch-examples-findings.md): `=> 1` directly
// followed by `(Left(_), Right(_))` fuses into `1(Left(_), Right(_))`, a
// nonsense call, corrupting every arm after it. Single-constructor patterns
// (`Right(_)`, `Left(x)`) don't start with `(`, so nesting instead of
// tupling sidesteps the whole class of the bug rather than working around
// one instance of it.
comparePre(a: Either<Str, ()>, b: Either<Str, ()>) -> Int =
    match a {
        Right(_) => match b { Right(_) => 0  Left(_) => 1 }
        Left(x) => match b { Right(_) => 0 - 1  Left(y) => compareTags(x, y) }
    }

compareTags(x: Str, y: Str) -> Int = x < y => 0 - 1 | (x > y => 1 | 0)

// `sortBy` takes a scalar key, and a version's ordering isn't reducible to
// one (numeric major/minor/patch, then a separate release-vs-prerelease
// then lexicographic-tag rule) — so this is a hand-written insertion sort
// against `compareVersions` instead of forcing the comparison through
// `sortBy`'s key-only shape.
sortVersions(versions: List<Version>) -> List<Version> = fold(versions, [], insertSorted)

insertSorted(sorted: List<Version>, v: Version) -> List<Version> =
    match first(sorted) {
        None => [v]
        Some(head) =>
            compareVersions(v, head) <= 0
                => concat([v], sorted)
                | concat([head], insertSorted(List.removeAt(sorted, 0), v))
    }

// --- tests ---

test "parseVersion reads a plain release version" {
    expect parseVersion("1.2.3") == Ok(Version(1, 2, 3, Right(())))
}

test "parseVersion reads a prerelease tag" {
    expect parseVersion("1.2.3-alpha") == Ok(Version(1, 2, 3, Left("alpha")))
}

test "parseVersion rejects the wrong number of components" {
    match parseVersion("1.2") {
        Err(_) => expect true
        Ok(_) => expect false
    }
}

test "parseVersion rejects non-numeric components" {
    match parseVersion("1.x.3") {
        Err(_) => expect true
        Ok(_) => expect false
    }
}

test "showVersion formats a release and a prerelease" {
    expect showVersion(Version(1, 2, 3, Right(()))) == "1.2.3"
    expect showVersion(Version(1, 2, 3, Left("beta"))) == "1.2.3-beta"
}

test "compareVersions orders by major, then minor, then patch" {
    expect compareVersions(Version(2, 0, 0, Right(())), Version(1, 9, 9, Right(()))) > 0
    expect compareVersions(Version(1, 2, 0, Right(())), Version(1, 3, 0, Right(()))) < 0
    expect compareVersions(Version(1, 2, 3, Right(())), Version(1, 2, 4, Right(()))) < 0
    expect compareVersions(Version(1, 2, 3, Right(())), Version(1, 2, 3, Right(()))) == 0
}

test "a release outranks a prerelease at the same major.minor.patch" {
    expect compareVersions(Version(1, 0, 0, Right(())), Version(1, 0, 0, Left("alpha"))) > 0
    expect compareVersions(Version(1, 0, 0, Left("alpha")), Version(1, 0, 0, Right(()))) < 0
}

test "sortVersions orders a mixed list ascending" {
    let versions = [
        Version(1, 0, 0, Right(())),
        Version(0, 9, 0, Right(())),
        Version(1, 0, 0, Left("rc1")),
        Version(1, 0, 1, Right(())),
    ]
    expect sortVersions(versions) |> map(showVersion) == [
        "0.9.0", "1.0.0-rc1", "1.0.0", "1.0.1",
    ]
}
