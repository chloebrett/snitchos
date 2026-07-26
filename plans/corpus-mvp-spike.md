# Corpus MVP — Increment 0, the spike

**Status: 📐 PLAN — not started.** Increment 0 of
[corpus-mvp.md](corpus-mvp.md). Related:
[corpus-recipe-axes.md](corpus-recipe-axes.md) (the recipes used below),
[../docs/language-design.md](../docs/language-design.md) (source for the
cheat-sheet), [stitch-examples-corpus.md](stitch-examples-corpus.md) (where
better exemplars come from).

**The deliverable is a decision and four numbers, not code.** Roughly two hours,
most of it pasting.

---

## What this answers

1. **Which model.** The primary output.
2. **Is the plan viable at all** — the floor check.
3. **Build order** — do Increments 7 and 8 come before the first real run?
4. **Prompt v2** — what obviously fails in v1. Most of the value is here, because
   yield dominates every downstream cost.

## What it cannot answer

**The yield number.** Twenty samples per model puts binomial noise at roughly
±10 percentage points, so this cannot distinguish 20% from 35%. It gives an order
of magnitude and a floor. Precise yield comes from the first real batch; do not
tune anything against these numbers as if they were tight.

---

## Setup

### S1. Models — fix the family, vary only size

Install **Qwen3-4B, Qwen3-8B and Qwen3-14B** (or another single family across
three sizes). Holding the family fixed makes this a controlled comparison about
*size*; mixing families confounds size with training data. Family is a separate
axis and would need its own twenty.

**Use the text-only variant, not `-vl`.** Vision-language builds spend a large
share of their post-training on multimodal data, which dilutes code ability
against the text-only sibling at the same parameter count. There are no images
here, so the VL tax buys nothing — and if one size in the comparison is VL and
another is not, the size comparison is confounded.

Configuration, all easy to get wrong and expensive:

- **Thinking mode off.** Thinking tokens are discarded here and can triple
  time-per-candidate.
- **Sampling: temp ≈ 0.7, top_p ≈ 0.8, top_k ≈ 20** (Qwen3's non-thinking
  recommendations), plus **repetition penalty ≈ 1.05–1.1**. Near-greedy decoding
  is the classic route into a repetition collapse — see Findings 001.
- **`max_tokens` ≈ 1200.** A 40–70 line program is ~600–900 tokens. Without a cap,
  a looping candidate consumes whatever context remains.
- **Pin all of the above now**, and write them on the record sheet. Later
  comparisons against a bulk run are meaningless if these drift.

### S2. Exemplar pool

Usable today: `stats.st` (111), `text.st` (70), `prelude.st` (108),
`double.st`, `primes.st`. **Exclude `plans/lang/samples.st`** (illustrative
fragments, not programs). **`stim.st` (890) and `json.st` (381) are too large to
paste** — they would dominate prefill.

That leaves a thin pool, which is the point of S3.

### S3. The cheat-sheet

**Written — it is the reference block inside Prompt v1 below.** Syntax verified
against [../docs/language-design.md](../docs/language-design.md) rather than
inferred from the exemplars, because a cheat-sheet that teaches wrong syntax is
worse than none.

**This is the highest-value artifact in the spike.** With the pool in S2, most
constructs have no exemplar demonstrating them, so the cheat-sheet carries nearly
all the syntax load until the 30 programs land. Example-based, not prose — models
follow examples better than descriptions.

### S4. The one piece of code allowed

`stitch` has no `src/bin`, so there is no `stitch check foo.st`. The only
zero-code path is dropping candidates into `examples/stitch/` and running the
test suite, which is all-or-nothing and pollutes the repo.

Pull forward the first half of Increment 1: **`verdict(src) -> Verdict` plus a
~20-line main that reads a path.** Sixty manual inspections become sixty
one-line commands.

> **Tripwire.** If this grows candidate extraction, a recipe loader, or anything
> that calls a model, the harness has arrived early through the side door. Stop
> and go back to pasting. The budget is the gate function plus a `main`.

---

## Prompt v1

Rendered for recipe **#69 sauna booking** — chosen as the first paste because
`prod`, `Maybe` and `|>` are all visible in both exemplars, making it the fairest
possible first test. Swap the final section to run any other recipe; everything
above it is invariant and belongs in the cached prefix.

**System:**

````
You write Stitch, a small statically-typed functional language. You have not
seen Stitch before — learn it from the reference and examples that follow.

Rules that are easy to get wrong:
- There are no loop keywords. Use recursion or List/Seq combinators.
- There is no if/else. Conditionals are `cond => a | b`, or a `match { }` block.
- Exported items are prefixed `ext`; everything else is module-private.
- `->` always means "maps to", `=>` always "case/condition", `|` always
  "alternation".
- Comments explain *why*, never *what*.
- Include `test "…" { expect … }` items covering the core logic.

Reply with exactly one fenced ```stitch block and nothing else.
````

**User:**

````
# Stitch reference

// Modules & visibility
use List                                 // import a module
use text.{pad, wrap}                     // import named items
ext foo(...)                             // exported; without `ext`, module-private

// Data — fields are immutable unless marked `mut`
prod Point(x: Int, y: Int)
prod Counter(mut n: Int)
sum Shape = Circle(Int) | Square(Int)
sum Maybe<T>     = Some(T) | None        // built in
sum Result<T, E> = Ok(T)   | Err(E)      // built in

// Construction uses named fields
Point(x: 1, y: 2)

// Functions — expression body or block body
ext double(n: Int) -> Int = n * 2
ext describe(n: Int) -> Str = {
    let d = double(n)
    d > 10 => "big" | "small"
}

// Conditionals
cond => thenValue | elseValue
match {
    n == 0 => "zero"
    n > 0  => "positive"
    _      => "negative"
}

// Methods attach with `on`; `@` is the receiver, `@x` is field x
on Counter {
    bumped() -> Counter = Counter(n: @n + 1)
}
on Counter : Drawable {                  // `: Contract` declares conformance
    draw() uses Canvas = renderBar(@n)
}

// Capabilities are declared on the signature
report(xs: List<Int>) uses Telemetry = ...

// Short-circuit family
value?                                   // unwrap a Result/Maybe or return early
user?.address                            // safe navigation

// Pipes, lambdas, placeholders
xs |> map(x -> x * 2) |> fold(0, (a, b) -> a + b)
xs |> map($.name)                        // `$a`/`$b` are positional placeholders

// Tests are ordinary items
test "double doubles" { expect double(2) == 4 }

# Example programs

==== BANK.ST ====


use Str

// --- accounts ---

contract Show {
    show() -> Str
}

prod Account(id: Str, owner: Str, mut balance: Int)

on Account {
    // Returns the updated `Account` — not because callers usually need it
    // (the write-back to `@` already updates whatever variable this was
    // called on; see `Ledger.deposit` below, which never touches the return
    // value), but because `Ledger`'s own methods *do* need it, to carry the
    // update from a local `let mut acc = …` binding into `@accounts`.
    mut deposit(amount: Int) -> Result<Account, Str> uses Telemetry {
        amount <= 0
            => Err("deposit amount must be positive")
            | {
                @balance = @balance + amount
                emit("account.deposit", amount)
                Ok(@)
            }
    }

    mut withdraw(amount: Int) -> Result<Account, Str> uses Telemetry {
        amount <= 0
            => Err("withdrawal amount must be positive")
            | (amount > @balance
                => Err("insufficient funds")
                | {
                    @balance = @balance - amount
                    emit("account.withdraw", amount)
                    Ok(@)
                })
    }
}

on Account : Show {
    show() -> Str = @owner + " (" + @id + "): $" + toStr(@balance)
}

// --- the ledger ---

sum TxKind = Deposit | Withdraw | TransferOut(Str) | TransferIn(Str)

prod Transaction(account: Str, kind: TxKind, amount: Int)

prod Ledger(mut accounts: List<Account>, mut history: List<Transaction>)

newLedger() -> Ledger = Ledger([], [])

on Ledger {
    mut openAccount(id: Str, owner: Str, opening: Int) -> Account = {
        let acc = Account(id, owner, opening)
        @accounts = concat(@accounts, [acc])
        acc
    }

    findAccount(id: Str) -> Maybe<Account> = find(@accounts, a -> a.id == id)

    mut record(tx: Transaction) { @history = concat(@history, [tx]) }

    // Depositing/withdrawing at the ledger level: find the account (a fresh
    // local binding, so `mut`-method write-back has somewhere to land),
    // mutate *that* binding, then fold the change back into the list. The
    // ledger-level `Result` never needs `acc.deposit(...)`'s own return
    // value — the write-back already updated `acc` in place.
    mut deposit(id: Str, amount: Int) -> Result<(), Str> uses Telemetry {
        let mut acc = (@findAccount(id) |> okOr("no such account: " + id))?
        acc.deposit(amount)?
        @accounts = replaceAccount(@accounts, acc)
        Ok(())
    }

    mut withdraw(id: Str, amount: Int) -> Result<(), Str> uses Telemetry {
        let mut acc = (@findAccount(id) |> okOr("no such account: " + id))?
        acc.withdraw(amount)?
        @accounts = replaceAccount(@accounts, acc)
        Ok(())
    }

    // Both accounts are checked to exist *before* anything moves — if the
    // recipient doesn't exist, the sender is never debited. (An earlier
    // draft withdrew first and deposited second; a missing recipient made
    // the withdrawn amount vanish, since nothing here can roll a mutation
    // back once it's landed. There is no transaction/rollback primitive in
    // the language, so a multi-step mutation has to be made safe by
    // ordering and up-front validation, by hand, every time.)
    mut transfer(fromId: Str, toId: Str, amount: Int) -> Result<(), Str> uses Telemetry {
        // `let _ = …` rather than two bare statements: a statement that
        // starts with `(` right after a preceding expression-statement
        // fuses with it into a call (`stmt1  (stmt2)` parses as
        // `stmt1(stmt2)` — maximal munch, see the findings doc). `let`
        // starting the line rules that out.
        let _ = (@findAccount(fromId) |> okOr("no such account: " + fromId))?
        let _ = (@findAccount(toId) |> okOr("no such account: " + toId))?
        @withdraw(fromId, amount)?
        @deposit(toId, amount)?
        @record(Transaction(fromId, TransferOut(toId), amount))
        @record(Transaction(toId, TransferIn(fromId), amount))
        emit("ledger.transfer", amount)
        Ok(())
    }

    // Note: no `uses Telemetry` here, only `FsWrite` — `@transfer` carries
    // its *own* `uses Telemetry` clause and gets that authority independent
    // of what this method declares (a named call's authority is exactly its
    // own `uses` row, never inherited from or filtered by the caller — see
    // `stitch/src/natives.rs`'s `shout()/main()` test for the same rule from
    // the refusal side). `auditLine` below needs `FsWrite` the same way.
    mut transferAudited(fromId: Str, toId: Str, amount: Int, auditHandle: Int) -> Result<(), Str> uses FsWrite {
        @transfer(fromId, toId, amount)?
        auditLine(auditHandle, fromId + "->" + toId + ":" + toStr(amount))
        Ok(())
    }
}

// The list-level half of "value semantics forces an explicit write-back":
// replace the account sharing `updated`'s id, leave every other element
// untouched.
replaceAccount(accounts: List<Account>, updated: Account) -> List<Account> =
    map(accounts, a -> a.id == updated.id => updated | a)

// The `Maybe -> Result` lift `?` needs and the prelude doesn't provide
// (only `Maybe`'s own `None`/`Some` short-circuit via the `Try` contract —
// this is what turns "absent" into a *named* failure).
okOr(m, err) = match m { Some(v) => Ok(v)  None => Err(err) }

report(ledger: Ledger) -> Str =
    ledger.accounts |> map(a -> a.show()) |> Str.join("\n")

totalBalance(ledger: Ledger) -> Int =
    ledger.accounts |> map(a -> a.balance) |> total

// Explicit-capability audit logging: `Ledger`'s own methods never declare
// `FsWrite` for their core bookkeeping (see `deposit`/`withdraw`/`transfer`
// above), so only a caller that reaches for `transferAudited` — and holds a
// file handle to hand it — pays for, and grants, that authority. The
// parameter isn't named `handle` — that's a reserved word (`handle op with
// f { … }`, the effect-handler form; see plans/stitch-examples-findings.md).
// `fileHandle` is a capability the caller must already hold (see
// `natives.rs::native_fs_write`); there is no "open this path for writing"
// native, only writing through an already-delegated handle, so a caller
// can't name an arbitrary file, only use one it was given.
auditLine(fileHandle: Int, entry: Str) -> Bool uses FsWrite = fsWrite(fileHandle, entry)

// --- tests ---

test "openAccount adds a findable account with the opening balance" {
    let mut ledger = newLedger()
    ledger.openAccount("a1", "Alice", 100)
    expect ledger.findAccount("a1") == Some(Account("a1", "Alice", 100))
    expect ledger.findAccount("missing") == None
}

test "two accounts built the same way are structurally equal" {
    expect Account("a1", "Alice", 100) == Account("a1", "Alice", 100)
    expect Account("a1", "Alice", 100) != Account("a1", "Alice", 101)
}

test "deposit increases the balance and is visible through findAccount" {
    let mut ledger = newLedger()
    ledger.openAccount("a1", "Alice", 100)
    ledger.deposit("a1", 50)
    expect ledger.findAccount("a1") == Some(Account("a1", "Alice", 150))
}

test "deposit rejects a non-positive amount and leaves the balance alone" {
    let mut ledger = newLedger()
    ledger.openAccount("a1", "Alice", 100)
    expect ledger.deposit("a1", 0) == Err("deposit amount must be positive")
    expect ledger.deposit("a1", 0 - 5) == Err("deposit amount must be positive")
    expect ledger.findAccount("a1") == Some(Account("a1", "Alice", 100))
}

test "deposit to a missing account is an error naming the id" {
    let mut ledger = newLedger()
    expect ledger.deposit("ghost", 10) == Err("no such account: ghost")
}

test "withdraw decreases the balance when funds are sufficient" {
    let mut ledger = newLedger()
    ledger.openAccount("a1", "Alice", 100)
    ledger.withdraw("a1", 30)
    expect ledger.findAccount("a1") == Some(Account("a1", "Alice", 70))
}

test "withdraw refuses insufficient funds and leaves the balance alone" {
    let mut ledger = newLedger()
    ledger.openAccount("a1", "Alice", 100)
    expect ledger.withdraw("a1", 101) == Err("insufficient funds")
    expect ledger.findAccount("a1") == Some(Account("a1", "Alice", 100))
}

test "withdraw rejects a non-positive amount" {
    let mut ledger = newLedger()
    ledger.openAccount("a1", "Alice", 100)
    expect ledger.withdraw("a1", 0) == Err("withdrawal amount must be positive")
}

test "a mut method call writes back into its own local binding without needing the return value" {
    // The point: `acc.deposit(50)` is called for its write-back effect on
    // `acc`, and `Ledger.deposit` above never captures the `Ok(Account)` it
    // returns — this test proves that write-back actually happened, using
    // the same shape directly rather than through the ledger.
    let mut acc = Account("a1", "Alice", 100)
    acc.deposit(50)
    expect acc == Account("a1", "Alice", 150)
}

test "transfer moves the balance and records both sides of the history" {
    let mut ledger = newLedger()
    ledger.openAccount("a1", "Alice", 100)
    ledger.openAccount("a2", "Bob", 10)
    ledger.transfer("a1", "a2", 40)
    expect ledger.findAccount("a1") == Some(Account("a1", "Alice", 60))
    expect ledger.findAccount("a2") == Some(Account("a2", "Bob", 50))
    expect ledger.history == [
        Transaction("a1", TransferOut("a2"), 40),
        Transaction("a2", TransferIn("a1"), 40),
    ]
}

test "transfer with insufficient funds changes neither account" {
    let mut ledger = newLedger()
    ledger.openAccount("a1", "Alice", 10)
    ledger.openAccount("a2", "Bob", 10)
    expect ledger.transfer("a1", "a2", 999) == Err("insufficient funds")
    expect ledger.findAccount("a1") == Some(Account("a1", "Alice", 10))
    expect ledger.findAccount("a2") == Some(Account("a2", "Bob", 10))
}

test "transfer to a missing recipient never debits the sender" {
    let mut ledger = newLedger()
    ledger.openAccount("a1", "Alice", 100)
    expect ledger.transfer("a1", "ghost", 40) == Err("no such account: ghost")
    expect ledger.findAccount("a1") == Some(Account("a1", "Alice", 100))
    expect ledger.history == []
}

test "show renders owner, id, and balance" {
    expect Account("a1", "Alice", 150).show() == "Alice (a1): $150"
}

test "report joins every account's show line" {
    let mut ledger = newLedger()
    ledger.openAccount("a1", "Alice", 100)
    ledger.openAccount("a2", "Bob", 10)
    expect report(ledger) == "Alice (a1): $100\nBob (a2): $10"
}

test "totalBalance sums every account" {
    let mut ledger = newLedger()
    ledger.openAccount("a1", "Alice", 100)
    ledger.openAccount("a2", "Bob", 10)
    ledger.openAccount("a3", "Cy", 0)
    expect totalBalance(ledger) == 110
}

test "transferAudited moves the balance and attempts the audit write" uses FsWrite {
    let mut ledger = newLedger()
    ledger.openAccount("a1", "Alice", 100)
    ledger.openAccount("a2", "Bob", 10)
    // The default interpreter platform has no filesystem (`NullPlatform`,
    // see `stitch/src/platform.rs`), so `fsWrite` — and so `auditLine` —
    // returns `false` here: "no filesystem" is a normal outcome, not a
    // fault, and this test is really about the capability wiring compiling
    // and running at all, not about bytes landing anywhere.
    expect ledger.transferAudited("a1", "a2", 40, 1) == Ok(())
    expect ledger.findAccount("a1") == Some(Account("a1", "Alice", 60))
    expect auditLine(1, "standalone entry") == false
}

==== JSON.ST ====

use Str

// --- the AST ---

sum Json =
    | JNull
    | JBool(Bool)
    | JNum(Int)
    | JStr(Str)
    | JArr(List<Json>)
    | JObj(List<(Str, Json)>)

// Look up a key in a parsed object. `None` for a missing key *or* for a
// non-object — the caller asked "what's under this key", and a scalar has no
// keys, so the two failure shapes are the same answer.
jsonGet(j: Json, key: Str) -> Maybe<Json> =
    match j {
        JObj(fields) => find(fields, pair -> at(pair, 0) == key) |> mapMaybe(pair -> at(pair, 1))
        _ => None
    }

// Tuple field access by position — there is no `.0`/`.1` on a tuple (the only
// projection is pattern-matching one apart), so this is the reusable version
// of `match t { (a, b) => a }`.
at(pair: (Str, Json), i: Int) -> Json = match pair { (a, b) => i == 0 => a | b }

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
        => Ok((JObj([]), afterBrace + 1))
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
                "}}" => Ok((JObj(grown), after + 1))
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
    expect parse(" {{ \"a\" : 1 ,  \"b\" : 2 }} ") == Ok(JObj([("a", JNum(1)), ("b", JNum(2))]))
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
    let doc = JObj([("name", JStr("stitch")), ("count", JNum(3)), ("tags", JArr([JStr("a"), JStr("b")])), ("ok", JBool(true)), ("nil", JNull)])
    expect parse(print(doc)) == Ok(doc)
}

test "print escapes quotes and backslashes" {
    expect print(JStr("a\"b\\c")) == "\"a\\\"b\\\\c\""
}

test "print renders negative numbers" {
    expect print(JNum(0 - 12)) == "-12"
}

==== END EXAMPLES ====

# Your task

Write a sauna booking module: rooms are booked for exclusive use over a time
window, so the core of the problem is detecting when two bookings overlap.

Shape: a module — `ext` items, no `main`.
Size: about 40–70 lines.
Use these constructs: prod, Maybe, |>
Use these words as identifiers somewhere meaningful: cedar, loyly, cooldown
````

### Reading the first result

- **Judge meaning, not validity.** Does it actually detect interval overlap, or
  is it a record store with a filter? A parse failure is the *least* interesting
  outcome; a program that parses and typechecks while being about nothing is the
  result that should worry you.
- **The three jargon words are a live test** of the must-use-words finding.
  `cooldown` lands easily and `cedar` is a plausible room name, but `loyly` has no
  home unless the model reaches into the domain. `let loyly = 0` dead filler on
  prompt one would confirm the failure mode early and cheaply.
- **Time prefill separately from decode.** Reference plus two exemplars is
  ~2,600 tokens, and that number is exactly what Increment 4's prefix caching
  exists to amortise.

---

## The eight recipes

Drawn from [corpus-recipe-axes.md](corpus-recipe-axes.md), balanced two-per-shape
— **deliberately over-sampling `script`**, which is only 5 of 100 in the seed
table and would otherwise go untested.

| # | Domain (clause) | Constructs | Size | Shape | Words |
|---|---|---|---|---|---|
| 28 | taxi meter — state machine over distance and waiting time, with tariff changes | sum, prod, uses Telemetry | small | script | fare, surcharge, hiring |
| 88 | brewery fermentation — time series against target curves, with alerts | prod, contract+on, uses Telemetry | large | script | krausen, gravity, pitch |
| 69 | sauna booking — exclusive occupancy, so overlapping intervals must be detected | prod, Maybe, \|> | small | module | cedar, loyly, cooldown |
| 58 | go territory scoring — flood-fill regions; distinguish alive groups from dead | sum, recursion, Map, \|> | large | module | liberty, seki, atari |
| 27 | level crossing barrier — a safety interlock state machine | sum, contract+on, uses Telemetry | medium | server loop | interlock, approach, wigwag |
| 98 | laundromat machine status — cycle timing and queue notification | sum, prod, uses Telemetry | medium | server loop | drum, lint, cycle |
| 46 | petty cash box — receipts must reconcile the float | prod, Result+? | small | library-with-heavy-tests | chit, imprest, float |
| 54 | bowling scorecard — strikes and spares pull forward later frames | sum, recursion | medium | library-with-heavy-tests | frame, spare, tenth |

**Run each two to three times per model** (≈20 per model, 60 total). Repeats are
not waste: within-recipe variance is itself a finding. A recipe that goes 0/3 on
every model is a recipe problem; 1/3 everywhere is noise you need to know about
before reading anything else.

Render each as a **brief**, not an axis list — per Increment 4:

> Write a tides API: a server loop that answers queries against a tide table.

not

> Domain: tide table / Shape: server loop

---

## Procedure

For each of 60 candidates:

1. Build the prompt: system + cheat-sheet + 2–3 exemplars + rendered brief.
2. Paste, generate, note **decode tok/s** as reported by the runner.
3. Save the extracted program to a file; run the S4 checker.
4. Record the row.

### Record sheet

One table, kept comparable across models. Note the sampling params once at the
top.

| id | model | recipe | tok/s | verdict | death detail | note |
|---|---|---|---|---|---|---|
| 01 | qwen3-4b | 28 | | ok / type / parse / extract | | |

`extract` means no usable fenced block came back — a prompt failure, not a
Stitch failure, and worth separating because the fix is different.

### Also record, once per model

- Ten programs eyeballed: **does this mean anything, or is it Stitch-shaped and
  saying nothing?** This is the judgement no metric substitutes for and it is the
  deciding input.
- Anything the model got wrong *consistently* — that is prompt v2's backlog.

---

## Decision rules — fix these before looking at results

Pre-registered so the numbers cannot be rationalised after the fact.

**Floor check.** At least one program parses across all 60 → proceed. Zero → stop
and reconsider; per corpus-mvp §7, a program parse rate of zero implies per-token
legality low enough that constrained decoding would puppet an ignorant model
rather than confirm a capable one.

**Model choice.** Highest **type-pass** count wins. Ties go to the smaller model
on throughput. Parse-pass is not a tiebreaker — Increment 7 flattens it to ~100%
for every model, so it cannot discriminate.

**The size override.** If the 14B's type-pass is more than ~2× the 4B's, the
yield gap may beat the ~7× speed gap. Recompute the validated-tokens-per-second
table with *measured* numbers rather than assuming small wins. Memory is not a
constraint at 64 GB, so every size stays available.

**Build order.** Compute `500_000 / yield / throughput`. Beyond ~16 hours, build
Increments 7 and 8 before the first real run — 7 first, since it is the larger
lever and removes the wasted candidates 8 would otherwise carry.

**The semantic red flag.** High parse-pass with near-zero type-pass everywhere
means the model has the shape and not the meaning. That is a cheat-sheet and
exemplar problem, and it must be fixed before any harness — a harness would only
industrialise the production of Stitch-shaped nonsense.

---

## Findings

Running log. One entry per candidate worth learning from — not all sixty.

### 001 — qwen3-vl-4b, recipe #69 sauna booking

| | |
|---|---|
| Model | qwen3-vl-4b, 16k context |
| Exemplar | `bank.st` |
| Throughput | **58.6 tok/s** single instance |
| Generated | 1589 tokens, did not stop on its own |
| Verdict | **parse** — but see below |

**What went right, and it is most of the interesting part.** The first ~25 lines
are structurally sound Stitch and semantically on-target:

```
prod Booking(start: Int, end: Int)
prod Room(mut bookings: List<Booking>)
on Room {
    mut book(start: Int, end: Int) -> Maybe<Booking> =
        let overlaps = find(@bookings, b -> start < b.end and end > b.start)
```

`start < b.end and end > b.start` is the textbook interval-overlap predicate. It
picked the right data shapes, used `mut` correctly, put behaviour on an `on`
block, and reached for a combinator rather than a loop. **The distinguishing
clause worked** — this is the domain's real computation, not a record store with
a filter.

**What went wrong.**

1. `prod Booking(start: Int, end: Int) = // …` — a spurious `=` after a `prod`
   declaration, twice.
2. `@b.end` where `b` is a lambda parameter — conflated the receiver sigil with
   general field access. `@` is only ever the receiver.
3. Inverted logic: both branches of the overlap check return `Some`, and the
   branch that appends is the one taken *when an overlap exists*.
4. **Terminal repetition loop** — ~1300 tokens of one comment repeated verbatim.

**Diagnosing the loop, in order of likely contribution:**

- **Sampling.** Near-greedy decoding with no repetition penalty and no
  `max_tokens`. With 16k context and a ~4k prompt it had ~12k tokens of room to
  spiral. Fixed in S1.
- **The comment instruction is hostile here.** "Comments explain *why*, never
  *what*" combined with `bank.st`, whose comments are multi-line rationale
  essays, taught *write extended prose justification*. Prose carries no syntactic
  obligation to terminate; a function body must close.
- **Comments are the escape hatch when the model is lost.** It derailed
  immediately after writing `mut book(...) = let newBooking = …` — a bare `let`
  sequence with no `{ }` block, a form the exemplars never show. Having produced
  an ungrammatical body it had no legal continuation in mind, and fell back on the
  one construct that is always legal. The loop is what a syntax dead-end looks
  like, not a comment problem.

**Bound on Increment 7, worth recording before it is built:** constrained
decoding would **not** have stopped this loop, because comments are grammatically
legal everywhere. It would have prevented the spurious `=` and the malformed body
that caused the dead-end — so it addresses the cause and not the symptom.
Repetition penalty and a token cap are needed regardless of the mask.

**The per-token legality read is good, and it is the reason to proceed.** Roughly
4–6 bad tokens out of ~300 code tokens ≈ **98% per-token legality** — the band
where the mask confirms a capable model rather than puppeting an ignorant one. By
corpus-mvp §7's table, 98% per-token predicts a program-level parse rate near
zero at this length, which is exactly what was observed. A parse failure here is
the amplification effect working as described, not evidence against the approach.

**Throughput confirms the model.** 58.6 tok/s sits inside the predicted 50–90
band, so the bandwidth model holds and the wall-clock table stands (~12 h for 500k
validated at 20% yield, single stream).

**Actions before candidate 002:**

- Apply the S1 sampling block (temp/top_p/top_k, repetition penalty,
  `max_tokens`).
- Switch to the **text-only** 4B, not `-vl`.
- Swap `bank.st` for `stats.st` + `text.st` — smaller, and less extreme comment
  style. Change one thing at a time: exemplars *and* sampling together would
  leave the cause ambiguous.
- Soften the comment line in the system prompt toward brevity.
- **New harness item, carried to Increment 5:** an n-gram repetition detector that
  kills generation early. This candidate burned ~1300 tokens after it was already
  dead; at scale that is a meaningful share of a run.

---

## Not doing

**Building the runner.** The pull is real and it is a trap: sixty pastes is an
hour, the runner is a day, and the runner cannot answer anything the pastes
cannot. Building it after means building it against known numbers instead of
guesses.

**Tuning the prompt mid-spike.** Note what fails, finish the sixty, then write
prompt v2 once. Editing between candidates makes the sixty incomparable and
destroys the only measurement this increment exists to produce.

**Judging the recipes.** A program that fails here is data about the model or the
prompt. The axes are not on trial in this increment.
