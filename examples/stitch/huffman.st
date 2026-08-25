// huffman.st — Huffman coding: build a frequency table, grow a weighted
// binary tree from it, derive a prefix-free bit code per symbol, then
// encode/decode. The round-trip (`decode(encode(text, codes), tree) ==
// text`) is the test that actually matters here — any bug in either
// direction shows up as corrupted text, not a subtly-wrong number.
//
// Frequencies are a `Map<Str, Int>` and the code table a `Map<Str, Str>`.
// Both were association lists (`List<Freq>`/`List<Code>` plus hand-written
// linear scans) until `Map` became buildable; the code table is the one that
// mattered, since `encodeChars` looked its symbol up once *per character of
// input* — a scan that is now a `Map.get`.
// `sortBy(xs, key)` (used to repeatedly pick the two lightest subtrees) is
// key-based, not comparator-based — pass the projection, not a `<`.

use Str
use List
use Map

// `Str.split(s, "")` is not "the characters of `s`" — it's a direct pass-
// through to Rust's `str::split`, whose documented behavior for an empty
// pattern includes an empty-string piece *before the first char and after
// the last* (`"abc".split("")` is 5 pieces: `["", "a", "b", "c", ""]`, and
// `"".split("")` is 2: `["", ""]`). Filtering those out is what makes this
// actually "the characters of `s`". It's silently harmless in a
// transform-then-rejoin use (json.st's `escapeStr` does this un-filtered —
// mapping any per-char transform over `""` and rejoining reproduces `""`,
// so the extra pieces vanish on their own), but corrupts anything that
// *counts* or *indexes* characters, which is exactly what this file does.
chars(s: Str) -> List<Str> = Str.split(s, "") |> filter(c -> c != "")

sum HuffTree = Leaf(Str, Int) | Node(HuffTree, HuffTree, Int)

weight(t: HuffTree) -> Int = match t { Leaf(_, w) => w  Node(_, _, w) => w }

frequencies(text: Str) -> Map<Str, Int> =
    fold(chars(text), [:], (acc, c) -> Map.update(acc, c, 1, $ + 1))

// The classic greedy build: repeatedly combine the two lightest trees into
// one until a single tree remains. `None` for no input at all; a
// single-symbol input degenerates to one bare `Leaf` (no `Node` at all) —
// `codesOf`/`decode` below both special-case that, since a tree with no
// branch point can't derive a code by walking branches.
buildTree(freqs: Map<Str, Int>) -> Maybe<HuffTree> =
    reduceTrees(map(freqs, f -> match f { (char, n) => Leaf(char, n) }))

reduceTrees(trees: List<HuffTree>) -> Maybe<HuffTree> =
    count(trees) == 0
        => None
        | (count(trees) == 1
            => first(trees)
            | reduceTrees(combineLightest(trees)))

combineLightest(trees: List<HuffTree>) -> List<HuffTree> = {
    let sorted = sortBy(trees, weight)
    // `unwrapOr` here is unreachable in practice: `combineLightest` is only
    // ever called with `count(trees) >= 2` (checked by `reduceTrees` above),
    // so both `at(0)`/`at(1)` are always `Some` — but there's no partial
    // "definitely non-empty" list type to say that statically, so the total
    // form is what's written.
    let a = List.at(sorted, 0) |> unwrapOr(Leaf("", 0))
    let b = List.at(sorted, 1) |> unwrapOr(Leaf("", 0))
    let rest = List.removeAt(List.removeAt(sorted, 0), 0)
    concat([Node(a, b, weight(a) + weight(b))], rest)
}

// A single-`Leaf` tree has no branches to spell a code from — it gets the
// fixed one-bit code `"0"` (one bit per occurrence is the only sensible
// encoding when there's only one symbol to distinguish from nothing).
//
// The walk collects `(symbol, bits)` pairs and `Map.fromList` turns them into
// the lookup table at the end — cheaper than merging a `Map` at every `Node`,
// and there is no `Map.merge` to do it with anyway.
codesOf(tree: HuffTree) -> Map<Str, Str> =
    match tree {
        Leaf(c, _) => Map.fromList([(c, "0")])
        Node(_, _, _) => Map.fromList(walkCodes(tree, ""))
    }

walkCodes(tree: HuffTree, prefix: Str) -> List<(Str, Str)> =
    match tree {
        Leaf(c, _) => [(c, prefix)]
        Node(l, r, _) => concat(walkCodes(l, prefix + "0"), walkCodes(r, prefix + "1"))
    }

encode(text: Str, codes: Map<Str, Str>) -> Result<Str, Str> = encodeChars(chars(text), codes)

encodeChars(chars: List<Str>, codes: Map<Str, Str>) -> Result<Str, Str> =
    match first(chars) {
        None => Ok("")
        Some(c) => match Map.get(codes, c) {
            None => Err("no code for character '" + c + "'")
            Some(bits) => {
                let rest = encodeChars(List.removeAt(chars, 0), codes)?
                Ok(bits + rest)
            }
        }
    }

// The single-`Leaf` case decodes without walking anything: every bit is
// its own one-character code (`codesOf`'s `"0"` above), so the decoded
// text is just that character repeated once per bit.
decode(bits: Str, tree: HuffTree) -> Str =
    match tree {
        Leaf(c, _) => repeatChar(c, Str.length(bits))
        Node(_, _, _) => decodeLoop(bits, 0, tree, tree, "")
    }

repeatChar(c: Str, n: Int) -> Str = n <= 0 => "" | c + repeatChar(c, n - 1)

decodeLoop(bits: Str, pos: Int, root: HuffTree, node: HuffTree, acc: Str) -> Str =
    match node {
        Leaf(c, _) => {
            let grown = acc + c
            pos >= Str.length(bits) => grown | decodeLoop(bits, pos, root, root, grown)
        }
        Node(l, r, _) =>
            pos >= Str.length(bits)
                => acc
                | (Str.slice(bits, pos, pos + 1) == "0"
                    => decodeLoop(bits, pos + 1, root, l, acc)
                    | decodeLoop(bits, pos + 1, root, r, acc))
    }

// --- tests ---

test "frequencies counts each distinct character" {
    expect frequencies("abracadabra") == ["a": 5, "b": 2, "r": 2, "c": 1, "d": 1]
    // `Map` equality is unordered; the `List<Freq>` this replaced asserted
    // first-seen order for free, so that half is now said explicitly.
    expect Map.keys(frequencies("abracadabra")) == ["a", "b", "r", "c", "d"]
}

test "a single-symbol text builds a bare Leaf, not a Node" {
    match buildTree(frequencies("aaaa")) {
        Some(Leaf(c, w)) => { expect c == "a"  expect w == 4 }
        _ => expect false
    }
}

test "codesOf gives every symbol a distinct prefix-free-by-construction code" {
    match buildTree(frequencies("abracadabra")) {
        None => expect false
        Some(tree) => {
            let codes = codesOf(tree)
            expect count(codes) == 5
            // No code is a prefix of another's — guaranteed structurally by
            // "every code is the path to a distinct leaf" rather than
            // checked here, but the count and shape are.
            expect Map.has(codes, "a")
        }
    }
}

test "the single-symbol tree encodes one bit per character" {
    match buildTree(frequencies("aaa")) {
        Some(tree) => {
            let codes = codesOf(tree)
            expect encode("aaa", codes) == Ok("000")
        }
        None => expect false
    }
}

test "encode fails with a named error for a character outside the tree" {
    match buildTree(frequencies("ab")) {
        Some(tree) => {
            match encode("abc", codesOf(tree)) {
                Err(_) => expect true
                Ok(_) => expect false
            }
        }
        None => expect false
    }
}

test "decode(encode(text)) reproduces the original text" {
    let text = "abracadabra"
    match buildTree(frequencies(text)) {
        None => expect false
        Some(tree) => {
            match encode(text, codesOf(tree)) {
                Err(_) => expect false
                Ok(bits) => expect decode(bits, tree) == text
            }
        }
    }
}

test "the round trip holds for a single-symbol text too" {
    let text = "zzzzz"
    match buildTree(frequencies(text)) {
        None => expect false
        Some(tree) => {
            match encode(text, codesOf(tree)) {
                Err(_) => expect false
                Ok(bits) => expect decode(bits, tree) == text
            }
        }
    }
}

test "buildTree on empty input is None" {
    expect buildTree(frequencies("")) == None
}
