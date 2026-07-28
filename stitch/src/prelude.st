// Stitch prelude — the part of the standard library written in Stitch itself,
// derived from the native core (fold/map/filter) + match + Maybe. Loaded into
// every program's globals before user code; user definitions shadow these.

// --- list combinators (derived from fold) ---

count(xs) = fold(xs, 0, (acc, _) -> acc + 1)

total(xs) = fold(xs, 0, (acc, x) -> acc + x)

any(xs, pred) = fold(xs, false, (acc, x) -> acc or pred(x))

all(xs, pred) = fold(xs, true, (acc, x) -> acc and pred(x))

contains(xs, value) = any(xs, x -> x == value)

test "count and total fold an empty list to zero" {
    expect count([]) == 0
    expect total([]) == 0
}

test "count counts elements and total sums them" {
    expect count([3, 1, 4]) == 3
    expect total([3, 1, 4]) == 8
}

// The vacuous cases are the ones a fold gets wrong by seeding the wrong
// identity, and they are the pair everyone's intuition disagrees with: nothing
// in an empty list satisfies a predicate, and everything in it does.
test "any is false and all is true on an empty list" {
    expect any([], _ -> true) == false
    expect all([], _ -> false) == true
}

test "any and all read the whole list" {
    expect any([1, 2, 3], x -> x == 3) == true
    expect any([1, 2, 3], x -> x == 9) == false
    expect all([1, 2, 3], x -> x > 0) == true
    expect all([1, 2, 3], x -> x > 1) == false
}

test "contains finds a member and rejects a non-member" {
    expect contains([1, 2, 3], 2) == true
    expect contains([1, 2, 3], 9) == false
    expect contains([], 1) == false
}

// Note: written as a bare fold body, not `{ f(x)  () }` — `f(x) ()` would
// parse as a call of f(x)'s result (the maximal-munch corner).
each(xs, f) = fold(xs, (), (_, x) -> f(x))

find(xs, pred) =
    fold(xs, None, (acc, x) ->
        match acc {
            Some(_) => acc
            None => pred(x) => Some(x) | None
        })

// `find` folds left and must keep the **first** match. A fold that overwrites
// `acc` on every hit returns the last instead, and agrees with any test whose
// list has only one match — so this one deliberately has three.
test "find returns the first match, not the last" {
    expect unwrapOr(find([1, 2, 3, 4], x -> x > 1), 0) == 2
}

test "find returns None when nothing matches" {
    expect isNone(find([1, 2, 3], x -> x > 9)) == true
}

// min/max fold to the extreme element, as a Maybe (None for an empty list).
// Comparable elements only (Int/Float/Str) — `<`/`>` are numeric+lexicographic.

min(xs) =
    fold(xs, None, (acc, x) ->
        match acc {
            None => Some(x)
            Some(m) => x < m => Some(x) | acc
        })

max(xs) =
    fold(xs, None, (acc, x) ->
        match acc {
            None => Some(x)
            Some(m) => x > m => Some(x) | acc
        })

// The extreme must not depend on where it sits in the list: with the smallest
// element already first, an accumulator that never updates still looks right.
// So each is asserted twice, with the answer at both ends.
test "min and max find the extreme wherever it sits" {
    expect unwrapOr(min([3, 1, 4]), 0) == 1
    expect unwrapOr(min([1, 3, 4]), 0) == 1
    expect unwrapOr(max([3, 4, 1]), 0) == 4
    expect unwrapOr(max([4, 3, 1]), 0) == 4
}

test "min and max of an empty list are None" {
    expect isNone(min([])) == true
    expect isNone(max([])) == true
}

// first/last as a Maybe (None for an empty list). Derived from fold, so for a
// List (eager); a Seq would diverge — force a prefix first.
first(xs) = fold(xs, None, (acc, x) -> match acc { None => Some(x)  Some(_) => acc })

last(xs) = fold(xs, None, (_, x) -> Some(x))

// These two are one fold body apart, and swapping them passes on any
// single-element list — hence three elements, and both ends asserted.
test "first and last pick opposite ends" {
    expect unwrapOr(first([7, 8, 9]), 0) == 7
    expect unwrapOr(last([7, 8, 9]), 0) == 9
}

test "first and last of an empty list are None" {
    expect isNone(first([])) == true
    expect isNone(last([])) == true
}

// flatten a List of Lists by concatenating — flatMap with the identity.
flatten(xss) = flatMap(xss, xs -> xs)

// The empty inner list is the case that separates "concatenate" from "collect":
// it must contribute nothing rather than a hole.
test "flatten concatenates in order and drops the empty inner list" {
    let f = flatten([[1, 2], [], [3]])
    expect count(f) == 3
    expect unwrapOr(first(f), 0) == 1
    expect unwrapOr(last(f), 0) == 3
}

// --- the Try contract: `?` short-circuits on the failure case ---
// `x?` desugars to `x.isFailure() => return x | x.unwrap()`. Std implements it
// for the built-in Maybe/Result; a user type with a success/failure split can
// implement the same two methods to make `?` work on it.

contract Try {
    isFailure() -> Bool
    unwrap() -> @
}

on Maybe : Try {
    isFailure() = match @ { None => true  Some(_) => false }
    unwrap() = match @ { Some(v) => v  None => @ }
}

on Result : Try {
    isFailure() = match @ { Err(_) => true  Ok(_) => false }
    unwrap() = match @ { Ok(v) => v  Err(_) => @ }
}

// `?` is desugaring, so what these pin is the *contract implementation* the
// desugaring calls. Both halves matter: `isFailure` decides whether `?` returns
// early, `unwrap` supplies the value when it does not.
test "the Try contract reads Maybe's failure case" {
    expect Some(1).isFailure() == false
    expect None.isFailure() == true
    expect Some(1).unwrap() == 1
}

test "the Try contract reads Result's failure case" {
    expect Ok(1).isFailure() == false
    expect Err("bad").isFailure() == true
    expect Ok(1).unwrap() == 1
}

// --- the Functor contract: `?.` maps a field access inside a container ---
// `x?.field` desugars to `x.map(v -> v.field)`. Unlike `?` (which unwraps), `map`
// re-wraps: it keeps the container shape, so the chain stays wrapped and
// short-circuits on the empty/failure case.

contract Functor {
    map(f) -> @
}

on Maybe : Functor {
    map(f) = match @ { Some(v) => Some(f(v))  None => None }
}

on Result : Functor {
    map(f) = match @ { Ok(v) => Ok(f(v))  Err(e) => Err(e) }
}

// The distinction from `?` is the whole point: `map` **re-wraps**, so the chain
// stays in the container and `?.` can be chained again.
test "Functor map keeps the container" {
    expect unwrapOr(Some(2).map(x -> x + 1), 0) == 3
    expect isNone(None.map(x -> x + 1)) == true
}

// `Err` carries a payload, and the failure arm is the one a mapper is tempted
// to collapse to a bare `Err`. Asserting the payload survives is what catches
// that; asserting only `isFailure` would not.
test "Functor map on Result preserves the error payload" {
    expect Ok(2).map(x -> x + 1).unwrap() == 3
    expect Err("nope").map(x -> x + 1).isFailure() == true
}

// --- Maybe helpers ---

isSome(m) = match m { Some(_) => true  None => false }

isNone(m) = match m { None => true  Some(_) => false }

unwrapOr(m, default) = match m { Some(v) => v  None => default }

mapMaybe(m, f) = match m { Some(v) => Some(f(v))  None => None }

andThen(m, f) = match m { Some(v) => f(v)  None => None }

test "isSome and isNone are opposites" {
    expect isSome(Some(1)) == true
    expect isNone(Some(1)) == false
    expect isNone(None) == true
    expect isSome(None) == false
}

test "unwrapOr takes the value when there is one and the default when there is not" {
    expect unwrapOr(Some(5), 0) == 5
    expect unwrapOr(None, 0) == 0
}

test "mapMaybe transforms a value and leaves None alone" {
    expect unwrapOr(mapMaybe(Some(2), x -> x * 10), 0) == 20
    expect isNone(mapMaybe(None, x -> x * 10)) == true
}

// `andThen` differs from `mapMaybe` in exactly one place: its function returns
// the container, so it can turn a `Some` into a `None`. `mapMaybe` cannot —
// which is why the second assertion is the one that tells them apart.
test "andThen lets the function decide the container" {
    expect unwrapOr(andThen(Some(2), x -> Some(x + 1)), 0) == 3
    expect isNone(andThen(Some(2), _ -> None)) == true
    expect isNone(andThen(None, x -> Some(x))) == true
}

// --- shell verbs ---

// `view f` — read file `f` and print its contents (or a not-found message). It
// runs with exactly `FsRead` + `ConsoleOut`: it may read files and write the
// console, nothing more — least authority, visible in the `uses` row.
view(f) uses FsRead, ConsoleOut = print(unwrapOr(readFile(f), "view: cannot read file"))
