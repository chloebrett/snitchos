// Stitch prelude — the part of the standard library written in Stitch itself,
// derived from the native core (fold/map/filter) + match + Maybe. Loaded into
// every program's globals before user code; user definitions shadow these.

// --- list combinators (derived from fold) ---

count(xs) = fold(xs, 0, (acc, _) -> acc + 1)

total(xs) = fold(xs, 0, (acc, x) -> acc + x)

any(xs, pred) = fold(xs, false, (acc, x) -> acc or pred(x))

all(xs, pred) = fold(xs, true, (acc, x) -> acc and pred(x))

contains(xs, value) = any(xs, x -> x == value)

// Note: written as a bare fold body, not `{ f(x)  () }` — `f(x) ()` would
// parse as a call of f(x)'s result (the maximal-munch corner).
each(xs, f) = fold(xs, (), (_, x) -> f(x))

find(xs, pred) =
    fold(xs, None, (acc, x) ->
        match acc {
            Some(_) => acc
            None => pred(x) => Some(x) | None
        })

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

// --- Maybe helpers ---

isSome(m) = match m { Some(_) => true  None => false }

isNone(m) = match m { None => true  Some(_) => false }

unwrapOr(m, default) = match m { Some(v) => v  None => default }

mapMaybe(m, f) = match m { Some(v) => Some(f(v))  None => None }

andThen(m, f) = match m { Some(v) => f(v)  None => None }
