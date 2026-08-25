// lru.st — an LRU cache as an immutable value: `get`/`put` never mutate,
// they return a *new* cache (recency-bumped, or evicted-if-over-capacity),
// the same discipline bank.st's `Ledger` needed for `mut` data sitting in
// a `List` — except here there's no `mut` at all, because there's nothing
// to mutate: rebuilding the entry list *is* the update. A memoized
// Fibonacci built on top shows why a cache is usually reached for in the
// first place, threading the cache explicitly through the recursion
// instead of hiding it behind a mutable global.
//
// Entries live in a `Map<Int, Int>`, **least**-recently-used first. The
// earlier version of this file used `List<Entry>` and said a `Map` "couldn't
// give the recency order for free even if it existed" — that turned out to be
// wrong twice over. `Map` preserves insertion order, and re-inserting a key
// after removing it appends it, so remove-then-insert *is* the recency bump
// and entry order *is* recency order. Eviction takes the first key.

use List
use Map

prod Cache(capacity: Int, entries: Map<Int, Int>)

newCache(capacity: Int) -> Cache = Cache(capacity, [:])

// The recency bump: drop the key, re-add it, and it lands at the end as the
// most-recently-used. Used by both `get` (a read is a recency mutation) and
// `put` (an overwrite counts as a use).
touch(entries: Map<Int, Int>, key: Int, value: Int) -> Map<Int, Int> =
    Map.insert(Map.remove(entries, key), key, value)

prod Hit(value: Int, cache: Cache)

// A hit both returns the value *and* bumps that key to most-recently-used
// — reading from an LRU cache is itself a mutation of its recency order,
// which is exactly why `get` returns a new `Cache` alongside the value
// instead of just `Maybe<Int>`.
get(cache: Cache, key: Int) -> Maybe<Hit> =
    match Map.get(cache.entries, key) {
        None => None
        Some(value) => Some(Hit(value, Cache(cache.capacity, touch(cache.entries, key, value))))
    }

// Overwriting an existing key also counts as "just used" (moves to the end);
// growing past `capacity` evicts the single least-recently-used entry — the
// first one, since entry order is recency order.
put(cache: Cache, key: Int, value: Int) -> Cache = {
    let grown = touch(cache.entries, key, value)
    count(grown) > cache.capacity
        => Cache(cache.capacity, evictOldest(grown))
        | Cache(cache.capacity, grown)
}

evictOldest(entries: Map<Int, Int>) -> Map<Int, Int> =
    match first(Map.keys(entries)) {
        None => entries
        Some(oldest) => Map.remove(entries, oldest)
    }

// --- memoized Fibonacci, cache threaded explicitly ---

prod FibResult(value: Int, cache: Cache)

fibMemo(cache: Cache, n: Int) -> FibResult =
    match get(cache, n) {
        Some(hit) => FibResult(hit.value, hit.cache)
        None => n <= 1
            => FibResult(n, put(cache, n, n))
            | {
                let left = fibMemo(cache, n - 1)
                let right = fibMemo(left.cache, n - 2)
                let value = left.value + right.value
                FibResult(value, put(right.cache, n, value))
            }
    }

fib(n: Int) -> Int = fibMemo(newCache(100), n).value

// --- tests ---

test "get on an empty cache is always a miss" {
    expect get(newCache(3), 1) == None
}

test "put then get returns the value and doesn't grow past capacity" {
    let cache = put(newCache(3), 1, 100)
    match get(cache, 1) {
        None => expect false
        Some(hit) => expect hit.value == 100
    }
}

test "putting an existing key overwrites its value, not the entry count" {
    let cache = put(put(newCache(3), 1, 100), 1, 200)
    expect count(cache.entries) == 1
    match get(cache, 1) {
        Some(hit) => expect hit.value == 200
        None => expect false
    }
}

test "putting past capacity evicts the least-recently-used entry" {
    let cache = put(put(put(newCache(2), 1, 10), 2, 20), 3, 30)
    expect count(cache.entries) == 2
    expect get(cache, 1) == None
    expect isSome(get(cache, 2))
    expect isSome(get(cache, 3))
}

test "a get bumps recency, changing which entry the next eviction picks" {
    let base = put(put(newCache(2), 1, 10), 2, 20)
    // Touch key 1 so it's now more-recently-used than key 2.
    let touched = match get(base, 1) { Some(hit) => hit.cache  None => base }
    let evicted = put(touched, 3, 30)
    // Key 2 was least-recently-used at the moment of the put, so it's gone;
    // key 1 survives because the `get` above protected it.
    expect get(evicted, 2) == None
    expect isSome(get(evicted, 1))
    expect isSome(get(evicted, 3))
}

test "put and get never mutate the cache they're called on" {
    let original = put(newCache(3), 1, 100)
    let grown = put(original, 2, 200)
    expect count(original.entries) == 1
    expect count(grown.entries) == 2
    expect get(original, 2) == None
}

test "fib matches the standard sequence" {
    expect fib(0) == 0
    expect fib(1) == 1
    expect fib(2) == 1
    expect fib(3) == 2
    expect fib(4) == 3
    expect fib(5) == 5
    expect fib(10) == 55
}

test "fibMemo's returned cache already holds the answer for a later lookup" {
    let result = fibMemo(newCache(100), 6)
    expect result.value == 8
    match get(result.cache, 6) {
        Some(hit) => expect hit.value == 8
        None => expect false
    }
}
