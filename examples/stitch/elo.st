// elo.st — an Elo-style rating calculator, with two `contract RatingSystem`
// strategies: a textbook fixed-K-factor system, and a "margin of victory"
// variant (the shape real-world adaptations — football/basketball rating
// systems — actually use: a blowout should move ratings more than a
// narrow win).
//
// **The real Elo formula needs `10^(diff/400)`, and this interpreter has
// no exponentiation, no `sqrt`, no `Float` math of any kind** — not even
// the `Int <-> Float` conversion missing since json.st's entry would help,
// because the formula itself needs a genuine exponential, not just a cast.
// `natives.rs` has no `pow`/`exp`/`sqrt` under any name. So `expectedScorePerMille`
// below is a deliberate **integer, piecewise-linear approximation** of the
// logistic expected-score curve — a real technique (some game engines use
// exactly this to avoid floating point), not a shortcut taken quietly. Every
// score is scaled ×1000 ("per mille") throughout so the whole file stays in
// `Int`.

prod Player(name: Str, rating: Int)

// `margin` is the winning side's margin of victory (goals, points, …) —
// meaningless to `FixedK` (which ignores it) but load-bearing for
// `MarginScaled`. `Draw` carries no margin; there wasn't one.
sum MatchResult = Win(Int) | Loss(Int) | Draw

actualPerMille(result: MatchResult) -> Int =
    match result { Win(_) => 1000  Loss(_) => 0  Draw => 500 }

matchMargin(result: MatchResult) -> Int =
    match result { Win(m) => m  Loss(m) => m  Draw => 0 }

// Linear stand-in for the logistic curve: a 0-point gap is a 50/50 game: a
// ±800-point gap (and beyond) is treated as a near-certain outcome, clamped
// to [0, 1000] rather than running past it.
expectedScorePerMille(diff: Int) -> Int = {
    let raw = 500 + diff * 500 / 800
    raw < 0 => 0 | (raw > 1000 => 1000 | raw)
}

prod RatingDelta(a: Int, b: Int)

contract RatingSystem {
    delta(ratingA: Int, ratingB: Int, result: MatchResult) -> RatingDelta
}

// The standard system: everyone's rating moves by the same fraction `k` of
// their surprise (actual score minus expected score), win or lose.
prod FixedK(k: Int)

on FixedK : RatingSystem {
    delta(ratingA: Int, ratingB: Int, result: MatchResult) -> RatingDelta = {
        let expected = expectedScorePerMille(ratingA - ratingB)
        let actual = actualPerMille(result)
        let change = @k * (actual - expected) / 1000
        RatingDelta(change, 0 - change)
    }
}

// A staircase in place of the textbook `sqrt(margin + 1)` diminishing-
// returns curve (no `sqrt` native either) — a 1-goal win counts as a normal
// result, a rout counts for up to double.
marginMultiplierPerMille(margin: Int) -> Int =
    margin <= 0
        => 1000
        | (margin == 1
            => 1200
            | (margin <= 3 => 1500 | 2000))

prod MarginScaled(baseK: Int)

on MarginScaled : RatingSystem {
    delta(ratingA: Int, ratingB: Int, result: MatchResult) -> RatingDelta = {
        let expected = expectedScorePerMille(ratingA - ratingB)
        let actual = actualPerMille(result)
        let scaledK = @baseK * marginMultiplierPerMille(matchMargin(result)) / 1000
        let change = scaledK * (actual - expected) / 1000
        RatingDelta(change, 0 - change)
    }
}

prod MatchOutcome(a: Player, b: Player)

// Runs one match under `system` (any `RatingSystem`, dispatched
// dynamically — the third `contract`-strategy program in the batch, after
// sched.st and inventory.st) and returns both players with their ratings
// updated.
playMatch(a: Player, b: Player, result: MatchResult, system) -> MatchOutcome = {
    let d = system.delta(a.rating, b.rating, result)
    MatchOutcome(Player(a.name, a.rating + d.a), Player(b.name, b.rating + d.b))
}

// --- tests ---

test "expectedScorePerMille is 500 (a coin flip) at equal ratings" {
    expect expectedScorePerMille(0) == 500
}

test "expectedScorePerMille moves linearly with the rating gap" {
    expect expectedScorePerMille(400) == 750
    expect expectedScorePerMille(0 - 400) == 250
}

test "expectedScorePerMille clamps to [0, 1000] for a large gap" {
    expect expectedScorePerMille(1000) == 1000
    expect expectedScorePerMille(0 - 1000) == 0
}

test "actualPerMille reads 1000/0/500 for win/loss/draw" {
    expect actualPerMille(Win(1)) == 1000
    expect actualPerMille(Loss(1)) == 0
    expect actualPerMille(Draw) == 500
}

test "FixedK's delta is always zero-sum" {
    let d = FixedK(32).delta(1500, 1400, Win(0))
    expect d.a == 0 - d.b
}

test "FixedK: an equal-rated win is worth half the K-factor" {
    let d = FixedK(32).delta(1500, 1500, Win(0))
    expect d == RatingDelta(16, 0 - 16)
}

test "FixedK: beating a much-lower-rated opponent barely moves you" {
    let d = FixedK(32).delta(1600, 800, Win(0))
    expect d == RatingDelta(0, 0)
}

test "FixedK: an upset loss to a much-lower-rated opponent costs the full K" {
    let d = FixedK(32).delta(1600, 800, Loss(0))
    expect d == RatingDelta(0 - 32, 32)
}

test "MarginScaled amplifies a decisive win beyond FixedK's fixed swing" {
    let flat = FixedK(32).delta(1500, 1500, Win(3))
    let scaled = MarginScaled(32).delta(1500, 1500, Win(3))
    expect flat == RatingDelta(16, 0 - 16)
    expect scaled == RatingDelta(24, 0 - 24)
}

test "MarginScaled agrees with FixedK when the margin is zero" {
    expect MarginScaled(32).delta(1500, 1400, Win(0)) == FixedK(32).delta(1500, 1400, Win(0))
}

test "playMatch updates both players' ratings from one match" {
    let outcome = playMatch(Player("ada", 1500), Player("bo", 1500), Win(0), FixedK(32))
    expect outcome == MatchOutcome(Player("ada", 1516), Player("bo", 1484))
}

test "a draw between equal-rated players changes nothing" {
    let outcome = playMatch(Player("ada", 1500), Player("bo", 1500), Draw, FixedK(32))
    expect outcome == MatchOutcome(Player("ada", 1500), Player("bo", 1500))
}
