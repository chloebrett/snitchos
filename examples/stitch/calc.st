// calc.st — an infix arithmetic expression parser/evaluator with variables:
// `+ - * /`, parens, and named lookups. Distinct from json.st's parser in
// shape on purpose — this is the classic two-stage pipeline (tokenize, then
// a precedence-climbing recursive-descent parse), where json.st's is a
// single-pass character scanner. And unlike json.st (written before
// `Str.parseInt` existed), this one uses it directly for number tokens —
// see plans/stitch-examples-findings.md's json.st entry for why that
// native didn't exist yet when json.st was written.
//
// Deliberately out of scope: unary minus (`-5`, `-(1+2)`). Supporting it
// means deciding how `3 - -5` and `-5 * 2` parse, which is real but
// unrelated complexity for a file whose point is precedence climbing and
// `Result`-threaded errors, not a complete grammar. `parseAtom` simply has
// no case for a leading `TMinus`, so `-5` is a clean parse error rather
// than silently doing the wrong thing.

use Str
use List
use Map

// --- tokens ---

sum Token =
    | TNum(Int)
    | TIdent(Str)
    | TPlus
    | TMinus
    | TStar
    | TSlash
    | TLParen
    | TRParen

tokenize(src: Str) -> Result<List<Token>, Str> = scanTokens(src, 0, [])

scanTokens(src: Str, pos: Int, acc: List<Token>) -> Result<List<Token>, Str> = {
    let p = skipWs(src, pos)
    p >= Str.length(src)
        => Ok(reverse(acc))
        | {
            let c = Str.slice(src, p, p + 1)
            match c {
                "+" => scanTokens(src, p + 1, concat([TPlus], acc))
                "-" => scanTokens(src, p + 1, concat([TMinus], acc))
                "*" => scanTokens(src, p + 1, concat([TStar], acc))
                "/" => scanTokens(src, p + 1, concat([TSlash], acc))
                "(" => scanTokens(src, p + 1, concat([TLParen], acc))
                ")" => scanTokens(src, p + 1, concat([TRParen], acc))
                _ => (isDigit(c)
                    => scanNumber(src, p, acc)
                    | (isAlpha(c)
                        => scanIdent(src, p, acc)
                        | Err("unexpected character '" + c + "'")))
            }
        }
}

scanNumber(src: Str, pos: Int, acc: List<Token>) -> Result<List<Token>, Str> = {
    let end = scanWhile(src, pos, isDigit)
    match Str.parseInt(Str.slice(src, pos, end)) {
        None => Err("malformed number")
        Some(n) => scanTokens(src, end, concat([TNum(n)], acc))
    }
}

scanIdent(src: Str, pos: Int, acc: List<Token>) -> Result<List<Token>, Str> = {
    let end = scanWhile(src, pos, isAlpha)
    scanTokens(src, end, concat([TIdent(Str.slice(src, pos, end))], acc))
}

// The index just past the last consecutive char satisfying `pred`, starting
// at `pos`. Shared by number- and identifier-scanning, parameterized on
// which character class to keep consuming.
scanWhile(src: Str, pos: Int, pred) -> Int =
    pos >= Str.length(src) or not pred(Str.slice(src, pos, pos + 1))
        => pos
        | scanWhile(src, pos + 1, pred)

isDigit(c: Str) -> Bool =
    c == "0" or c == "1" or c == "2" or c == "3" or c == "4"
    or c == "5" or c == "6" or c == "7" or c == "8" or c == "9"

isAlpha(c: Str) -> Bool = (c >= "a" and c <= "z") or (c >= "A" and c <= "Z")

skipWs(src: Str, pos: Int) -> Int =
    pos >= Str.length(src)
        => pos
        | (isWs(Str.slice(src, pos, pos + 1)) => skipWs(src, pos + 1) | pos)

isWs(c: Str) -> Bool = c == " " or c == "\t" or c == "\n"

// --- the AST ---

sum Expr =
    | ENum(Int)
    | EVar(Str)
    | EAdd(Expr, Expr)
    | ESub(Expr, Expr)
    | EMul(Expr, Expr)
    | EDiv(Expr, Expr)

// --- parsing (precedence climbing) ---
//
// `parseAdd` (loosest) calls `parseMul` (tighter) calls `parseAtom`
// (tightest — literals, variables, and `(…)` grouping, which recurses back
// to `parseAdd`). Each level's `*Loop` helper is what makes `+`/`-`
// left-associative: it keeps folding `<left> op <next>` into a bigger left
// operand instead of recursing into the right, so `1 - 2 - 3` parses as
// `(1 - 2) - 3`, not `1 - (2 - 3)`.

prod Parsed(expr: Expr, pos: Int)

parse(src: Str) -> Result<Expr, Str> = {
    let tokens = tokenize(src)?
    let result = parseAdd(tokens, 0)?
    result.pos == count(tokens) => Ok(result.expr) | Err("trailing tokens after the expression")
}

parseAdd(tokens: List<Token>, pos: Int) -> Result<Parsed, Str> = {
    let left = parseMul(tokens, pos)?
    parseAddLoop(tokens, left)
}

parseAddLoop(tokens: List<Token>, acc: Parsed) -> Result<Parsed, Str> =
    match List.at(tokens, acc.pos) {
        Some(TPlus) => {
            let rhs = parseMul(tokens, acc.pos + 1)?
            parseAddLoop(tokens, Parsed(EAdd(acc.expr, rhs.expr), rhs.pos))
        }
        Some(TMinus) => {
            let rhs = parseMul(tokens, acc.pos + 1)?
            parseAddLoop(tokens, Parsed(ESub(acc.expr, rhs.expr), rhs.pos))
        }
        _ => Ok(acc)
    }

parseMul(tokens: List<Token>, pos: Int) -> Result<Parsed, Str> = {
    let left = parseAtom(tokens, pos)?
    parseMulLoop(tokens, left)
}

parseMulLoop(tokens: List<Token>, acc: Parsed) -> Result<Parsed, Str> =
    match List.at(tokens, acc.pos) {
        Some(TStar) => {
            let rhs = parseAtom(tokens, acc.pos + 1)?
            parseMulLoop(tokens, Parsed(EMul(acc.expr, rhs.expr), rhs.pos))
        }
        Some(TSlash) => {
            let rhs = parseAtom(tokens, acc.pos + 1)?
            parseMulLoop(tokens, Parsed(EDiv(acc.expr, rhs.expr), rhs.pos))
        }
        _ => Ok(acc)
    }

parseAtom(tokens: List<Token>, pos: Int) -> Result<Parsed, Str> =
    match List.at(tokens, pos) {
        None => Err("unexpected end of input")
        Some(TNum(n)) => Ok(Parsed(ENum(n), pos + 1))
        Some(TIdent(name)) => Ok(Parsed(EVar(name), pos + 1))
        Some(TLParen) => {
            let inner = parseAdd(tokens, pos + 1)?
            match List.at(tokens, inner.pos) {
                Some(TRParen) => Ok(Parsed(inner.expr, inner.pos + 1))
                _ => Err("expected ')'")
            }
        }
        Some(_) => Err("unexpected token")
    }

// --- evaluation ---

// The environment is a `Map<Str, Int>` — it was an association list plus a
// hand-written `find` until `Map` became buildable.
lookup(env: Map<Str, Int>, name: Str) -> Result<Int, Str> =
    match Map.get(env, name) {
        None => Err("unbound variable: " + name)
        Some(value) => Ok(value)
    }

eval(expr: Expr, env: Map<Str, Int>) -> Result<Int, Str> =
    match expr {
        ENum(n) => Ok(n)
        EVar(name) => lookup(env, name)
        EAdd(l, r) => evalBin(l, r, env, (a, b) -> Ok(a + b))
        ESub(l, r) => evalBin(l, r, env, (a, b) -> Ok(a - b))
        EMul(l, r) => evalBin(l, r, env, (a, b) -> Ok(a * b))
        EDiv(l, r) => evalBin(l, r, env, (a, b) -> b == 0 => Err("division by zero") | Ok(a / b))
    }

evalBin(l: Expr, r: Expr, env: Map<Str, Int>, f) -> Result<Int, Str> = {
    let lv = eval(l, env)?
    let rv = eval(r, env)?
    f(lv, rv)
}

run(src: Str, env: Map<Str, Int>) -> Result<Int, Str> = {
    let expr = parse(src)?
    eval(expr, env)
}

// --- tests ---

test "tokenize recognizes numbers, operators, and parens" {
    expect tokenize("1 + 2 * 3") == Ok([TNum(1), TPlus, TNum(2), TStar, TNum(3)])
    expect tokenize("(x - 1)") == Ok([TLParen, TIdent("x"), TMinus, TNum(1), TRParen])
}

test "tokenize errors on an unrecognized character" {
    expect tokenize("1 + @") == Err("unexpected character '@'")
}

test "tokenize ignores whitespace between tokens" {
    expect tokenize("  1   +2") == Ok([TNum(1), TPlus, TNum(2)])
}

test "multiplication binds tighter than addition" {
    expect run("1 + 2 * 3", [:]) == Ok(7)
    expect run("2 * 3 + 1", [:]) == Ok(7)
}

test "parens override precedence" {
    expect run("(1 + 2) * 3", [:]) == Ok(9)
}

test "subtraction and division are left-associative" {
    expect run("10 - 2 - 3", [:]) == Ok(5)
    expect run("100 / 5 / 2", [:]) == Ok(10)
}

test "variables resolve against the supplied environment" {
    let env = ["x": 2, "y": 5]
    expect run("x + y", env) == Ok(7)
    expect run("x * (y - 1)", env) == Ok(8)
}

test "an unbound variable is a named error, not a fault" {
    expect run("x + 1", [:]) == Err("unbound variable: x")
}

test "division by zero is a named error" {
    expect run("1 / 0", [:]) == Err("division by zero")
}

test "a syntax error is reported rather than silently truncating" {
    match run("1 +", [:]) {
        Err(_) => expect true
        Ok(_) => expect false
    }
}

test "trailing tokens after a complete expression are rejected" {
    expect run("1 + 2 3", [:]) == Err("trailing tokens after the expression")
}

test "unary minus is out of scope and reported as a syntax error" {
    match run("-5", [:]) {
        Err(_) => expect true
        Ok(_) => expect false
    }
}

test "an empty expression is an error, not zero" {
    match run("", [:]) {
        Err(_) => expect true
        Ok(_) => expect false
    }
}
