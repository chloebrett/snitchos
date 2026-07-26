// trie.st — a persistent prefix trie: `insert`/`lookup`/`prefixSearch`,
// every one of them returning (or searching) a *new* structure rather than
// mutating in place. A trie is the sharpest test yet of "no `Box` needed
// for a recursive type" (docs/language-design.md's "GC dividends") — `Trie`
// contains a `List<TrieEdge>` and `TrieEdge` contains a `Trie` right back,
// a mutual recursion that just works, the same way `sum List<T> =
// Cons(T, List<T>) | Nil` does.
//
// `children` is `List<TrieEdge>`, not `Map<Str, Trie>`, for the batch's by
// -now-familiar reason (json.st's entry, plans/stitch-examples-findings.md):
// no native builds a `Map` from a computed sequence of entries. A trie
// makes the cost of that concrete in a way earlier programs didn't — a
// 26-way branch is a 26-element linear scan here, where a real `Map` would
// be the whole point of a trie's fan-out. Filed as a *repeated* data point
// for that finding, not a new one.

use Str

prod TrieEdge(char: Str, node: Trie)
prod Trie(children: List<TrieEdge>, isWord: Bool)

empty() -> Trie = Trie([], false)

insert(trie: Trie, word: Str) -> Trie = insertAt(trie, word, 0)

insertAt(trie: Trie, word: Str, i: Int) -> Trie =
    i >= Str.length(word)
        => Trie(trie.children, true)
        | {
            let c = Str.slice(word, i, i + 1)
            match find(trie.children, e -> e.char == c) {
                None => {
                    let child = insertAt(empty(), word, i + 1)
                    Trie(concat(trie.children, [TrieEdge(c, child)]), trie.isWord)
                }
                Some(edge) => {
                    let updatedChild = insertAt(edge.node, word, i + 1)
                    Trie(map(trie.children, e -> e.char == c => TrieEdge(c, updatedChild) | e), trie.isWord)
                }
            }
        }

insertAll(words: List<Str>) -> Trie = fold(words, empty(), (t, w) -> insert(t, w))

lookup(trie: Trie, word: Str) -> Bool = lookupAt(trie, word, 0)

lookupAt(trie: Trie, word: Str, i: Int) -> Bool =
    i >= Str.length(word)
        => trie.isWord
        | {
            let c = Str.slice(word, i, i + 1)
            match find(trie.children, e -> e.char == c) {
                None => false
                Some(edge) => lookupAt(edge.node, word, i + 1)
            }
        }

// The subtree rooted just past `prefix`, or `None` if no inserted word
// shares it. `hasPrefix` and `prefixSearch` both reduce to "find this node,
// then look at (or under) it" — `nodeAt` is the one place that walk lives.
nodeAt(trie: Trie, prefix: Str, i: Int) -> Maybe<Trie> =
    i >= Str.length(prefix)
        => Some(trie)
        | {
            let c = Str.slice(prefix, i, i + 1)
            match find(trie.children, e -> e.char == c) {
                None => None
                Some(edge) => nodeAt(edge.node, prefix, i + 1)
            }
        }

hasPrefix(trie: Trie, prefix: Str) -> Bool = isSome(nodeAt(trie, prefix, 0))

// Every complete word reachable from `node`, each spelled out in full by
// carrying the prefix already walked to reach it.
wordsUnder(node: Trie, prefix: Str) -> List<Str> =
    concat(
        node.isWord => [prefix] | [],
        flatMap(node.children, e -> wordsUnder(e.node, prefix + e.char)),
    )

prefixSearch(trie: Trie, prefix: Str) -> List<Str> =
    match nodeAt(trie, prefix, 0) {
        None => []
        Some(node) => wordsUnder(node, prefix)
    }

// --- tests ---

test "lookup finds exactly the inserted words, not their prefixes" {
    let trie = insertAll(["cat", "car", "card"])
    expect lookup(trie, "cat") == true
    expect lookup(trie, "car") == true
    expect lookup(trie, "card") == true
    expect lookup(trie, "ca") == false
    expect lookup(trie, "ca rd") == false
    expect lookup(trie, "dog") == false
}

test "an empty trie finds nothing" {
    expect lookup(empty(), "anything") == false
    expect lookup(empty(), "") == false
}

test "the empty string can itself be inserted and found" {
    let trie = insert(empty(), "")
    expect lookup(trie, "") == true
}

test "hasPrefix is true for any prefix of an inserted word, complete or not" {
    let trie = insertAll(["cat", "dog"])
    expect hasPrefix(trie, "c") == true
    expect hasPrefix(trie, "ca") == true
    expect hasPrefix(trie, "cat") == true
    expect hasPrefix(trie, "catt") == false
    expect hasPrefix(trie, "d") == true
    expect hasPrefix(trie, "x") == false
}

test "prefixSearch returns every word under a prefix, shortest paths first" {
    let trie = insertAll(["cat", "car", "card", "dog"])
    expect prefixSearch(trie, "ca") == ["cat", "car", "card"]
    expect prefixSearch(trie, "d") == ["dog"]
}

test "prefixSearch on an unknown prefix is empty, not an error" {
    let trie = insertAll(["cat"])
    expect prefixSearch(trie, "xyz") == []
}

test "prefixSearch on the whole trie's root lists every word" {
    let trie = insertAll(["a", "an", "ant"])
    expect prefixSearch(trie, "") == ["a", "an", "ant"]
}

test "insert is persistent: the original trie is untouched by a later insert" {
    let original = insertAll(["cat"])
    let grown = insert(original, "car")
    expect lookup(original, "car") == false
    expect lookup(grown, "car") == true
    expect lookup(original, "cat") == true
}

test "inserting the same word twice changes nothing observable" {
    let trie = insertAll(["cat", "cat"])
    expect lookup(trie, "cat") == true
    expect prefixSearch(trie, "c") == ["cat"]
}
