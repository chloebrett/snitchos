// poker.st — rank a 5-card poker hand. Ranks are `2..14` (`14` = Ace,
// `11..13` = Jack/Queen/King) — no ace-low "wheel" straight (`A-2-3-4-5`),
// a deliberate scope cut rather than an oversight; it's the one common
// exception to "highest card decides," and this file's point is the
// grouping/sorting machinery, not full rule coverage.
//
// **`classifyByCount`/`classifyRest` below are nested `match`es on `Bool`/
// `Int`, deliberately not one `match (straight, flush, top.count, …) { … }`
// tuple-pattern match.** The first draft used exactly that tuple-pattern
// shape and hit the maximal-munch bug a *third* way (semver.st's entry,
// plans/stitch-examples-findings.md): a tuple-pattern arm whose body is a
// bare constructor call, immediately followed by the next arm's tuple
// pattern, fuses the call-followed-by-`(` gotcha into arm boundaries too.
// Nesting single-value matches (`match top.count { 4 => …  3 => … }`) never
// starts an arm with `(`, so it never triggers.

use List
use Map

prod Card(rank: Int, suit: Str)

sum HandRank =
    | HighCard(Int)
    | Pair(Int)
    | TwoPair(Int, Int)
    | ThreeOfAKind(Int)
    | Straight(Int)
    | Flush(Int)
    | FullHouse(Int, Int)
    | FourOfAKind(Int)
    | StraightFlush(Int)

max2(a: Int, b: Int) -> Int = a > b => a | b
min2(a: Int, b: Int) -> Int = a < b => a | b

prod RankCount(rank: Int, count: Int)

// The tally is `Map.update` over a fold. `RankCount` survives the arrival of
// `Map` on its own merits rather than as a workaround: `classifyByCount` reads
// `top.rank`/`top.count`/`second.rank`/`second.count` a dozen times, where a
// bare `(Int, Int)` entry would need a `match` at every use. Build with the
// dictionary, hand back the named pair.
groupByRank(cards: List<Card>) -> List<RankCount> =
    fold(cards, [:], (acc, c) -> Map.update(acc, c.rank, 1, $ + 1))
        |> Map.entries
        |> map(e -> match e { (rank, n) => RankCount(rank, n) })

// Every adjacent pair in `sortedRanks` differs by exactly 1 — rigorous
// against a hand with a pair (e.g. `2,2,3,4,6`, whose min/max also happen
// to differ by 4 but which isn't a straight): a duplicate breaks
// consecutiveness immediately, at the duplicate, not at the ends.
isConsecutive(sortedRanks: List<Int>) -> Bool =
    count(sortedRanks) <= 1
        => true
        | ((List.at(sortedRanks, 1) |> unwrapOr(0)) - (List.at(sortedRanks, 0) |> unwrapOr(0)) == 1
            and isConsecutive(List.removeAt(sortedRanks, 0)))

checkStraight(sortedRanks: List<Int>) -> Bool = count(sortedRanks) == 5 and isConsecutive(sortedRanks)

allSameSuit(cards: List<Card>) -> Bool = {
    let s0 = List.at(cards, 0) |> mapMaybe(c -> c.suit) |> unwrapOr("")
    all(cards, c -> c.suit == s0)
}

classify(cards: List<Card>) -> HandRank = {
    let ranks = sortBy(map(cards, c -> c.rank), r -> r)
    let straight = checkStraight(ranks)
    let flush = allSameSuit(cards)
    let counts = groupByRank(cards) |> sortBy(rc -> 0 - rc.count)
    let top = List.at(counts, 0) |> unwrapOr(RankCount(0, 0))
    let second = List.at(counts, 1) |> unwrapOr(RankCount(0, 0))
    let high = last(ranks) |> unwrapOr(0)
    classifyHand(straight, flush, top, second, high)
}

classifyHand(straight: Bool, flush: Bool, top: RankCount, second: RankCount, high: Int) -> HandRank =
    match straight and flush {
        true => StraightFlush(high)
        false => classifyByCount(straight, flush, top, second, high)
    }

classifyByCount(straight: Bool, flush: Bool, top: RankCount, second: RankCount, high: Int) -> HandRank =
    match top.count {
        4 => FourOfAKind(top.rank)
        3 => match second.count {
            2 => FullHouse(top.rank, second.rank)
            _ => classifyRest(flush, straight, high, ThreeOfAKind(top.rank))
        }
        2 => match second.count {
            2 => classifyRest(flush, straight, high, TwoPair(max2(top.rank, second.rank), min2(top.rank, second.rank)))
            _ => classifyRest(flush, straight, high, Pair(top.rank))
        }
        _ => classifyRest(flush, straight, high, HighCard(high))
    }

classifyRest(flush: Bool, straight: Bool, high: Int, fallback: HandRank) -> HandRank =
    match flush {
        true => Flush(high)
        false => match straight { true => Straight(high)  false => fallback }
    }

// --- tests ---

test "a straight flush outranks everything" {
    let hand = [Card(5, "H"), Card(6, "H"), Card(7, "H"), Card(8, "H"), Card(9, "H")]
    expect classify(hand) == StraightFlush(9)
}

test "four of a kind" {
    let hand = [Card(7, "H"), Card(7, "D"), Card(7, "C"), Card(7, "S"), Card(2, "H")]
    expect classify(hand) == FourOfAKind(7)
}

test "a full house" {
    let hand = [Card(9, "H"), Card(9, "D"), Card(9, "C"), Card(4, "S"), Card(4, "H")]
    expect classify(hand) == FullHouse(9, 4)
}

test "a flush (same suit, not consecutive)" {
    let hand = [Card(2, "S"), Card(5, "S"), Card(9, "S"), Card(11, "S"), Card(13, "S")]
    expect classify(hand) == Flush(13)
}

test "a straight (consecutive, mixed suits)" {
    let hand = [Card(4, "H"), Card(5, "D"), Card(6, "C"), Card(7, "S"), Card(8, "H")]
    expect classify(hand) == Straight(8)
}

test "three of a kind" {
    let hand = [Card(3, "H"), Card(3, "D"), Card(3, "C"), Card(9, "S"), Card(2, "H")]
    expect classify(hand) == ThreeOfAKind(3)
}

test "two pair" {
    let hand = [Card(10, "H"), Card(10, "D"), Card(5, "C"), Card(5, "S"), Card(2, "H")]
    expect classify(hand) == TwoPair(10, 5)
}

test "a single pair" {
    let hand = [Card(8, "H"), Card(8, "D"), Card(2, "C"), Card(5, "S"), Card(11, "H")]
    expect classify(hand) == Pair(8)
}

test "high card, nothing else" {
    let hand = [Card(2, "H"), Card(5, "D"), Card(9, "C"), Card(11, "S"), Card(13, "H")]
    expect classify(hand) == HighCard(13)
}

test "a same-suit pair with duplicate ranks isn't mistaken for a straight" {
    // Ranks 2,2,3,4,6: min/max differ by 4 (the naive straight check a
    // less careful `isConsecutive` might use), but there's no straight here.
    let hand = [Card(2, "H"), Card(2, "D"), Card(3, "C"), Card(4, "S"), Card(6, "H")]
    expect classify(hand) == Pair(2)
}
