// json.st — a hand-rolled JSON parser and printer.
//
// No external dependency, obviously (there isn't one to have) — the point is
// what a recursive-descent parser looks like in Stitch's own idiom: a `sum`
// AST, `Result<T, Str>` threaded by hand (there is no cursor/iterator type,
// so every parse step carries and returns its own position), and `match` for
// dispatch instead of a visitor.
//
// Two scope cuts, and both are language-shaped rather than arbitrary — see
// plans/stitch-examples-findings.md for the fuller writeup:
//
// - **Numbers are `Int` only.** There is still no `Str -> Float` or
//   `Int -> Float` conversion in the stdlib (v0 is strict — no implicit
//   numeric coercion), so a `3.14` in the input parses as a genuine parse
//   error here rather than silently truncating. `-42` and `0` parse fine.
//   (Number *parsing* itself uses `Str.parseInt` — a native that didn't
//   exist when this file was first written; see the findings doc for that
//   story. It closed the `Str -> Int` half of this gap, not the `Float`
//   half.)
// - **Objects are `Map<Str, Json>`.** They were `List<(Str, Json)>` — an
//   association list with a hand-written linear-scan `jsonGet` — until `Map`
//   became buildable from a runtime-computed sequence of pairs. `Map`
//   preserves insertion order, so parse→print round-trips field order the way
//   the association list did. One deliberate behaviour change: a duplicate key
//   in the input now resolves last-wins (`Map`'s rule, and JavaScript's)
//   instead of keeping both copies.

use Str
use Map

// --- the AST ---

sum Json =
    | JNull
    | JBool(Bool)
    | JNum(Int)
    | JStr(Str)
    | JArr(List<Json>)
    | JObj(Map<Str, Json>)

// Look up a key in a parsed object. `None` for a missing key *or* for a
// non-object — the caller asked "what's under this key", and a scalar has no
// keys, so the two failure shapes are the same answer.
jsonGet(j: Json, key: Str) -> Maybe<Json> =
    match j {
        JObj(fields) => Map.get(fields, key)
        _ => None
    }

// --- parsing ---
//
// Every step takes the source and a byte-index cursor and returns
// `Result<(T, Int), Str>` — the parsed value plus the index just past it, or
// an error message. Threading the cursor by hand (rather than through a
// mutable position) is what "immutable by default" costs a hand-written
// parser, and it is a fully ordinary cost: each step is still a pure
// function of `(src, pos)`.

// Parse a whole document: skip leading whitespace, parse one value, then
// demand nothing but trailing whitespace after it. A JSON document is
// exactly one value — `parse("1 2")` is an error, not "1".
parse(src: Str) -> Result<Json, Str> = {
    let start = skipWs(src, 0)
    match parseValue(src, start) {
        Err(e) => Err(e)
        Ok(pair) => {
            let value = at2(pair, 0)
            let rest = skipWs(src, at2(pair, 1))
            rest == Str.length(src) => Ok(value) | Err("trailing input after the document")
        }
    }
}

// Positional access into a `(Int, Int)`/`(Json, Int)`-shaped result pair.
// Named distinctly from `at` above only because the element type differs and
// Stitch has no generics-over-tuples to unify them.
at2(pair, i: Int) = match pair { (a, b) => i == 0 => a | b }

parseValue(src: Str, pos: Int) -> Result<(Json, Int), Str> =
    pos >= Str.length(src) => Err("unexpected end of input")
    | {
        let c = Str.slice(src, pos, pos + 1)
        match c {
            "n" => parseLiteral(src, pos, "null", JNull)
            "t" => parseLiteral(src, pos, "true", JBool(true))
            "f" => parseLiteral(src, pos, "false", JBool(false))
            "\"" => mapOkPair(parseStr(src, pos), s -> JStr(s))
            "[" => parseArr(src, pos)
            "{{" => parseObj(src, pos)
            _ => (c == "-" or isDigit(c)) => parseNum(src, pos) | Err("unexpected character '" + c + "'")
        }
    }

// `Result` has no `map` yet (only `Maybe` does, via the `Functor` contract) —
// this is that missing half, written by hand where a parser step needs it. A
// parse step always carries its cursor alongside the value, so this maps `f`
// over just the value half of an `Ok((value, pos))`.
mapOkPair(r, f) = match r { Ok(pair) => Ok((f(at2(pair, 0)), at2(pair, 1)))  Err(e) => Err(e) }

// `word` (e.g. `"null"`) starting at `pos`; succeeds with `value` if the
// source matches exactly, so `nulla` does not parse as `null` followed by an
// identifier — there is no identifier concept here, but `truee` should still
// be rejected as malformed rather than "true" plus garbage silently accepted
// by the outer `parse` trailing check. (It still is: `parse` rejects any
// trailing non-whitespace either way. This check only makes the local error
// message point at the right place.)
parseLiteral(src: Str, pos: Int, word: Str, value: Json) -> Result<(Json, Int), Str> = {
    let end = pos + Str.length(word)
    end <= Str.length(src) and Str.slice(src, pos, end) == word
        => Ok((value, end))
        | Err("expected '" + word + "'")
}

isDigit(c: Str) -> Bool =
    c == "0" or c == "1" or c == "2" or c == "3" or c == "4"
    or c == "5" or c == "6" or c == "7" or c == "8" or c == "9"

// Digit-scanning (`isDigit`/`scanDigits`) still has to be hand-rolled — it's
// how a "where does the number end" boundary gets found in a string with no
// tokenizer of its own — but turning the scanned slice into an `Int` doesn't:
// `Str.parseInt` (see plans/stitch-examples-findings.md) takes the whole
// slice, sign included, so there's no separate negative-number branch here
// either.
parseNum(src: Str, pos: Int) -> Result<(Json, Int), Str> = {
    let negative = Str.slice(src, pos, pos + 1) == "-"
    let digitsStart = negative => pos + 1 | pos
    digitsStart >= Str.length(src) or not isDigit(Str.slice(src, digitsStart, digitsStart + 1))
        => Err("expected a digit")
        | {
            let end = scanDigits(src, digitsStart)
            end < Str.length(src) and Str.slice(src, end, end + 1) == "."
                => Err("floating-point numbers are not supported (see the file header)")
                | match Str.parseInt(Str.slice(src, pos, end)) {
                    Some(n) => Ok((JNum(n), end))
                    None => Err("malformed number")
                }
        }
}

// The index just past the last consecutive digit starting at `pos`.
scanDigits(src: Str, pos: Int) -> Int =
    pos >= Str.length(src) or not isDigit(Str.slice(src, pos, pos + 1))
        => pos
        | scanDigits(src, pos + 1)

// A quoted string, `pos` pointing at the opening `"`. Supports the escapes
// that show up in real JSON text: `\"`, `\\`, `\/`, `\n`, `\t`, `\r`. No
// `\uXXXX` — that needs turning four hex digits into a character, and this
// interpreter has no code-point-to-`Str` native either (only the reverse
// direction is missing for `digitValue` above; this is its string-building
// cousin).
parseStr(src: Str, pos: Int) -> Result<(Str, Int), Str> =
    Str.slice(src, pos, pos + 1) != "\""
        => Err("expected '\"'")
        | scanStr(src, pos + 1, "")

scanStr(src: Str, i: Int, acc: Str) -> Result<(Str, Int), Str> =
    i >= Str.length(src)
        => Err("unterminated string")
        | {
            let c = Str.slice(src, i, i + 1)
            match c {
                "\"" => Ok((acc, i + 1))
                "\\" => {
                    i + 1 >= Str.length(src)
                        => Err("unterminated escape")
                        | {
                            let escaped = Str.slice(src, i + 1, i + 2)
                            match unescape(escaped) {
                                None => Err("unknown escape '\\" + escaped + "'")
                                Some(ch) => scanStr(src, i + 2, acc + ch)
                            }
                        }
                }
                _ => scanStr(src, i + 1, acc + c)
            }
        }

unescape(c: Str) -> Maybe<Str> =
    match c {
        "\"" => Some("\"")
        "\\" => Some("\\")
        "/"  => Some("/")
        "n"  => Some("\n")
        "t"  => Some("\t")
        "r"  => Some("\r")
        _    => None
    }

// `[` already at `pos`. `elems` are `,`-separated `parseValue`s, `]` closes;
// `[]` and trailing-comma-free (JSON has none) are both handled by the same
// "is the next non-ws char `]`?" check that starts and ends the loop.
parseArr(src: Str, pos: Int) -> Result<(Json, Int), Str> = {
    let afterBracket = skipWs(src, pos + 1)
    Str.slice(src, afterBracket, afterBracket + 1) == "]"
        => Ok((JArr([]), afterBracket + 1))
        | scanArr(src, afterBracket, [])
}

scanArr(src: Str, pos: Int, acc: List<Json>) -> Result<(Json, Int), Str> =
    match parseValue(src, pos) {
        Err(e) => Err(e)
        Ok(pair) => {
            let value = at2(pair, 0)
            let after = skipWs(src, at2(pair, 1))
            let next = Str.slice(src, after, after + 1)
            let grown = concat(acc, [value])
            match next {
                "," => scanArr(src, skipWs(src, after + 1), grown)
                "]" => Ok((JArr(grown), after + 1))
                _   => Err("expected ',' or ']' in array")
            }
        }
    }

// `{` already at `pos`. Mirrors `scanArr`, one indirection deeper: each
// element is a `"key": value` pair, and the pair itself threads its own
// cursor through `parseMember` before folding back into `scanObj`.
parseObj(src: Str, pos: Int) -> Result<(Json, Int), Str> = {
    let afterBrace = skipWs(src, pos + 1)
    Str.slice(src, afterBrace, afterBrace + 1) == "}}"
        => Ok((JObj([:]), afterBrace + 1))
        | scanObj(src, afterBrace, [])
}

scanObj(src: Str, pos: Int, acc: List<(Str, Json)>) -> Result<(Json, Int), Str> =
    match parseMember(src, pos) {
        Err(e) => Err(e)
        Ok(pair) => {
            let member = at2(pair, 0)
            let after = skipWs(src, at2(pair, 1))
            let next = Str.slice(src, after, after + 1)
            let grown = concat(acc, [member])
            match next {
                "," => scanObj(src, skipWs(src, after + 1), grown)
                // The scan accumulates a `List` of pairs and converts once at
                // the close brace — `Map.fromList` applies the duplicate-key
                // rule in one place rather than at every member.
                "}}" => Ok((JObj(Map.fromList(grown)), after + 1))
                _   => Err("expected ',' or '}}' in object")
            }
        }
    }

parseMember(src: Str, pos: Int) -> Result<((Str, Json), Int), Str> =
    match parseStr(src, pos) {
        Err(e) => Err(e)
        Ok(keyPair) => {
            let key = at2(keyPair, 0)
            let afterKey = skipWs(src, at2(keyPair, 1))
            Str.slice(src, afterKey, afterKey + 1) != ":"
                => Err("expected ':' after object key")
                | match parseValue(src, skipWs(src, afterKey + 1)) {
                    Err(e) => Err(e)
                    Ok(valuePair) => Ok(((key, at2(valuePair, 0)), at2(valuePair, 1)))
                }
        }
    }

// The index of the first non-whitespace character at or after `pos` (space,
// tab, `\n`, `\r` — the four JSON recognises).
skipWs(src: Str, pos: Int) -> Int =
    pos >= Str.length(src)
        => pos
        | (isWs(Str.slice(src, pos, pos + 1)) => skipWs(src, pos + 1) | pos)

isWs(c: Str) -> Bool = c == " " or c == "\t" or c == "\n" or c == "\r"

// --- printing ---
//
// The inverse direction. Not a byte-identical formatter (no attempt to
// preserve source whitespace — there is nothing to preserve, `Json` has
// already thrown it away) but a round-trip in the sense that matters:
// `parse(print(j))` reproduces `j`.

print(j: Json) -> Str =
    match j {
        JNull    => "null"
        JBool(b) => b => "true" | "false"
        JNum(n)  => toStr(n)
        JStr(s)  => "\"" + escapeStr(s) + "\""
        JArr(xs) => "[" + (xs |> map(print) |> Str.join(",")) + "]"
        JObj(fs) => "{{" + (fs |> map(printMember) |> Str.join(",")) + "}}"
    }

printMember(pair: (Str, Json)) -> Str = match pair { (k, v) => "\"" + escapeStr(k) + "\":" + print(v) }

escapeStr(s: Str) -> Str =
    Str.split(s, "")
        |> map(c -> match c {
            "\"" => "\\\""
            "\\" => "\\\\"
            "\n" => "\\n"
            "\t" => "\\t"
            _    => c
        })
        |> Str.join("")

// --- tests ---

test "parses the three literals" {
    expect parse("null") == Ok(JNull)
    expect parse("true") == Ok(JBool(true))
    expect parse("false") == Ok(JBool(false))
}

test "parses whole-number ints, including negative" {
    expect parse("0") == Ok(JNum(0))
    expect parse("42") == Ok(JNum(42))
    expect parse("-7") == Ok(JNum(0 - 7))
}

test "rejects a float, naming why" {
    match parse("3.14") {
        Err(_) => expect true
        Ok(_)  => expect false
    }
}

test "parses a string with escapes" {
    expect parse("\"hi\"") == Ok(JStr("hi"))
    expect parse("\"a\\nb\"") == Ok(JStr("a\nb"))
    expect parse("\"quote: \\\"x\\\"\"") == Ok(JStr("quote: \"x\""))
}

test "an unterminated string is an error, not a hang" {
    match parse("\"abc") {
        Err(_) => expect true
        Ok(_)  => expect false
    }
}

test "parses arrays, including empty and nested" {
    expect parse("[]") == Ok(JArr([]))
    expect parse("[1,2,3]") == Ok(JArr([JNum(1), JNum(2), JNum(3)]))
    expect parse("[[1],[2,3]]") == Ok(JArr([JArr([JNum(1)]), JArr([JNum(2), JNum(3)])]))
}

test "parses objects and jsonGet reaches a field" {
    let parsed = parse("{{\"a\":1,\"b\":[2,3]}}")
    match parsed {
        Err(_) => expect false
        Ok(doc) => {
            expect jsonGet(doc, "a") == Some(JNum(1))
            expect jsonGet(doc, "b") == Some(JArr([JNum(2), JNum(3)]))
            expect jsonGet(doc, "missing") == None
        }
    }
}

test "whitespace between tokens is ignored" {
    expect parse(" {{ \"a\" : 1 ,  \"b\" : 2 }} ") == Ok(JObj(["a": JNum(1), "b": JNum(2)]))
}

test "trailing garbage after the document is an error" {
    match parse("1 2") {
        Err(_) => expect true
        Ok(_)  => expect false
    }
}

test "an empty document is an error" {
    match parse("") {
        Err(_) => expect true
        Ok(_)  => expect false
    }
}

test "print round-trips through parse" {
    let doc = JObj([
        "name": JStr("stitch"),
        "count": JNum(3),
        "tags": JArr([JStr("a"), JStr("b")]),
        "ok": JBool(true),
        "nil": JNull,
    ])
    expect parse(print(doc)) == Ok(doc)
}

test "print escapes quotes and backslashes" {
    expect print(JStr("a\"b\\c")) == "\"a\\\"b\\\\c\""
}

test "print renders negative numbers" {
    expect print(JNum(0 - 12)) == "-12"
}
