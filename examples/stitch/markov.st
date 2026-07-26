// markov.st — a bigram Markov-chain text generator: tokenize a corpus,
// count what word follows what, then walk the chain with a seeded PRNG.
//
// Two things this file is the first in the batch to demonstrate:
//
// - **`use <-` (Gleam-style block sugar)**, in `generateWithTelemetry`
//   below — `fs-image/primes.st` already ships it, so this isn't new to the
//   *language*, just new to this batch.
// - **Direct tuple pattern-matching** (`match pair { (w1, w2) => … }` in
//   `buildChain`), which is *not* the `at2` workaround json.st needed.
//   `at2` was for a pair threaded across several function boundaries and
//   read repeatedly — a named `prod` communicates better there (see vm.st's
//   `Popped`). A pair destructured once, right where it's produced, is
//   exactly what `match` patterns are for; reaching for a named product
//   here would be the overcorrection.
//
// The chain itself is `List<Transition>` — an association list, not
// `Map<Str, List<Str>>` — for the now-familiar reason (plans/stitch-examples-findings.md,
// json.st's entry): building a `Map` from a computed, word-frequency-shaped
// sequence isn't expressible with today's literal-only `Map`.

use Str
use List

// --- tokenizing ---

tokenize(text: Str) -> List<Str> =
    Str.split(text, " ") |> filter(w -> w != "")

// --- the chain ---

prod Transition(word: Str, nexts: List<Str>)

// `nexts` deliberately keeps duplicates rather than deduping into counts: a
// word that follows `w1` three times out of five appears three times in
// `nexts`, so picking a uniformly random *index* already reproduces the
// corpus's own frequency weighting — no separate probability field needed.
buildChain(words: List<Str>) -> List<Transition> =
    // `zip(words, words-shifted-by-one)`. Reaching for `drop(words, 1)` here
    // first faults: `drop`/`dropWhile`/`take`/`takeWhile` are `Seq`-only
    // (unlike `fold`/`filter`/`map`, which special-case `List` too — see
    // `natives.rs`), and there's no `List -> Seq` conversion native to
    // bridge them. `List.removeAt(words, 0)` is the `List`-native way to
    // say "everything after the first element".
    fold(zip(words, List.removeAt(words, 0)), [], (acc, pair) ->
        match pair { (w1, w2) => addTransition(acc, w1, w2) })

addTransition(chain: List<Transition>, from: Str, to: Str) -> List<Transition> =
    match find(chain, t -> t.word == from) {
        None => concat(chain, [Transition(from, [to])])
        Some(_) => map(chain, t -> t.word == from => Transition(from, concat(t.nexts, [to])) | t)
    }

// `[]` for a word that never appears, *and* for a word that only ever
// appears last in the corpus (nothing was ever seen following it) — both
// are "no known continuation," the same answer either way.
transitionsOf(chain: List<Transition>, word: Str) -> List<Str> =
    find(chain, t -> t.word == word) |> mapMaybe(t -> t.nexts) |> unwrapOr([])

// --- a small seeded PRNG ---
//
// A linear congruential generator (glibc's constants) — plenty for picking
// an index into a short `nexts` list, and, being pure state-in-state-out
// rather than a hidden global, it composes with the rest of this file's
// immutable-by-default style: no `mut`, no ambient "current seed" anywhere.

prod Rng(seed: Int)

nextRng(r: Rng) -> Rng = Rng((r.seed * 1103515245 + 12345) % 2147483648)

prod Draw(value: Int, rng: Rng)

// A uniform value in `[0, bound)`, plus the advanced generator. `bound <= 0`
// is the caller's bug (there is no way to pick an index into a `nexts` list
// no one checked is non-empty first) — `generate` below always checks
// `count(nexts) == 0` before calling this, so that case is unreachable in
// practice rather than defended against here.
draw(r: Rng, bound: Int) -> Draw = {
    let advanced = nextRng(r)
    Draw(advanced.seed % bound, advanced)
}

// --- generation ---

generate(chain: List<Transition>, start: Str, steps: Int, rng: Rng) -> List<Str> =
    genLoop(chain, [start], start, steps, rng)

genLoop(chain: List<Transition>, acc: List<Str>, current: Str, stepsLeft: Int, rng: Rng) -> List<Str> =
    stepsLeft <= 0
        => reverse(acc)
        | {
            let nexts = transitionsOf(chain, current)
            count(nexts) == 0
                => reverse(acc)
                | {
                    let picked = draw(rng, count(nexts))
                    let word = List.at(nexts, picked.value) |> unwrapOr(current)
                    genLoop(chain, concat([word], acc), word, stepsLeft - 1, picked.rng)
                }
        }

// The `uses Telemetry` wrapper `run`/interactive use would actually call —
// `generate` above stays pure so it's cheap to call from tests without
// worrying about what capability a test needs to hold.
generateWithTelemetry(chain: List<Transition>, start: Str, steps: Int, rng: Rng) -> List<Str> uses Telemetry {
    use <- span("markov.generate")
    let result = generate(chain, start, steps, rng)
    emit("markov.generated.tokens", count(result))
    result
}

// --- tests ---

test "tokenize splits on spaces and drops empties from repeated ones" {
    expect tokenize("the cat sat") == ["the", "cat", "sat"]
    expect tokenize("a  b   c") == ["a", "b", "c"]
    expect tokenize("") == []
}

test "buildChain counts every bigram, keeping duplicates as weight" {
    let chain = buildChain(tokenize("the cat sat the cat ran the dog sat"))
    expect chain == [
        Transition("the", ["cat", "cat", "dog"]),
        Transition("cat", ["sat", "ran"]),
        Transition("sat", ["the"]),
        Transition("ran", ["the"]),
        Transition("dog", ["sat"]),
    ]
}

test "transitionsOf is empty for a word that never appears" {
    let chain = buildChain(tokenize("the cat sat"))
    expect transitionsOf(chain, "ghost") == []
}

test "transitionsOf is empty for a word only ever seen last" {
    let chain = buildChain(tokenize("hello world"))
    expect transitionsOf(chain, "world") == []
}

test "nextRng is a deterministic pure function of the seed" {
    expect nextRng(Rng(1)) == nextRng(Rng(1))
    expect nextRng(Rng(1)) != nextRng(Rng(2))
}

test "draw always lands inside [0, bound)" {
    let d1 = draw(Rng(42), 5)
    let d2 = draw(d1.rng, 5)
    let d3 = draw(d2.rng, 5)
    expect d1.value >= 0 and d1.value < 5
    expect d2.value >= 0 and d2.value < 5
    expect d3.value >= 0 and d3.value < 5
}

test "generate walks the chain deterministically for a given seed" {
    let chain = buildChain(tokenize("the cat sat the cat ran the dog sat"))
    expect generate(chain, "the", 5, Rng(42)) == ["the", "cat", "sat", "the", "cat", "ran"]
    expect generate(chain, "dog", 5, Rng(1)) == ["dog", "sat", "the", "cat", "ran", "the"]
}

test "generate stops early at a dead end instead of padding with garbage" {
    let chain = buildChain(tokenize("hello world"))
    // Whatever the seed, "world" has no continuation, so generation always
    // stops after exactly one step even though 5 were requested.
    expect generate(chain, "hello", 5, Rng(0)) == ["hello", "world"]
    expect generate(chain, "hello", 5, Rng(12345)) == ["hello", "world"]
}

test "generate from a word outside the corpus returns just that word" {
    let chain = buildChain(tokenize("the cat sat"))
    expect generate(chain, "ghost", 5, Rng(7)) == ["ghost"]
}

test "generateWithTelemetry returns the same walk as generate" uses Telemetry {
    let chain = buildChain(tokenize("the cat sat the cat ran the dog sat"))
    expect generateWithTelemetry(chain, "the", 5, Rng(42)) == ["the", "cat", "sat", "the", "cat", "ran"]
}
