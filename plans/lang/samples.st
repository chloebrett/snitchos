// Stitch — illustrative samples
//
// Pre-implementation: this is a FEEL document and a future parser/eval test corpus,
// not validated code. Syntax per plans/lang/01-grammar-and-precedence.md.

// ─────────────────────────────────────────────────────────────────────────
// Data modeling + pattern matching
// ─────────────────────────────────────────────────────────────────────────

sum Shape =
    | Circle(radius: Float)
    | Rect(w: Float, h: Float)
    | Triangle(base: Float, height: Float)

area(s: Shape) -> Float =
    match s {
        Circle(r)      => 3.14159 * r * r
        Rect(w, h)     => w * h
        Triangle(b, h) => b * h / 2.0
    }

// literals, guards, or-patterns, wildcard
classify(n: Int) -> Str =
    match n {
        0          => "zero"
        n if n < 0 => "negative"
        1 | 2 | 3  => "small"
        _          => "big"
    }

// ─────────────────────────────────────────────────────────────────────────
// Errors & absence — `?`, Result, Maybe, `?.`
// ─────────────────────────────────────────────────────────────────────────

prod Config(host: Str, port: Int, retries: Int)

// `?` threads the unhappy path; the happy path reads straight down.
parseConfig(raw: Map<Str, Str>) -> Result<Config, Str> {
    let host    = raw["host"] |> okOr("missing host")?
    let port    = raw["port"] |> okOr("missing port")? |> parseInt?
    let retries = raw["retries"] |> andThen(parseInt) |> unwrapOr(3)   // optional, default 3
    Ok(Config(host, port, retries))
}

prod Address(zip: Maybe<Str>)
prod User(name: Str, address: Maybe<Address>)

// safe navigation: None if address is None OR zip is None
zipOf(u: User) -> Maybe<Str> = u.address?.zip

// ─────────────────────────────────────────────────────────────────────────
// Pipelines — the everyday transform
// ─────────────────────────────────────────────────────────────────────────

prod Reading(sensor: Str, celsius: Float)

hotSensors(readings: List<Reading>, threshold: Float) -> List<Str> =
    readings
    |> filter($.celsius > threshold)
    |> map($.sensor)
    |> unique
    |> sort

prod Stats(count: Int, total: Float)

// the loop you'd write elsewhere, as a fold into a record
summarize(readings: List<Reading>) -> Stats =
    readings |> fold(Stats(0, 0.0), (acc, r) ->
        Stats(acc.count + 1, acc.total + r.celsius))

mean(s: Stats) -> Float =
    s.count == 0 => 0.0 | s.total / s.count.toFloat()

// word frequency — map literal `[:]`, grouped update
wordCounts(text: Str) -> Map<Str, Int> =
    text
    |> split(" ")
    |> map(lowercase)
    |> fold([:], (counts, w) ->
        counts |> update(w, (n) -> n |> unwrapOr(0) |> plus(1)))

// ─────────────────────────────────────────────────────────────────────────
// Lazy sequences & ranges
// ─────────────────────────────────────────────────────────────────────────

sumOfSquares(n: Int) -> Int =
    1..=n |> map($ * $) |> sum

// infinite range, lazily cut to n
firstEvens(n: Int) -> List<Int> =
    0.. |> filter($ % 2 == 0) |> take(n) |> toList

// ─────────────────────────────────────────────────────────────────────────
// Behavior — `on`, `@` receiver, `mut` methods, contracts, guard via `?`
// ─────────────────────────────────────────────────────────────────────────

contract Show {
    show() -> Str
}

prod Account(owner: Str, mut balance: Int)

on Account {
    mut deposit(amount: Int) uses Telemetry {
        @balance = @balance + amount
        emit("account.deposit", amount)
    }

    mut withdraw(amount: Int) -> Result<(), Str> {
        guard(amount <= @balance, "insufficient funds")?   // bail with Err if false
        @balance = @balance - amount
        Ok(())
    }
}

on Account : Show {
    show() -> Str = "{@owner}: ${@balance}"   // {expr} interpolates; the $ is a literal dollar sign
}

// ─────────────────────────────────────────────────────────────────────────
// A small program tying it together
// ─────────────────────────────────────────────────────────────────────────

main(caps: Startup) uses Net, Telemetry {
    use <- span("main")

    let readings = [
        Reading("a", 31.0),
        Reading("b", 19.5),
        Reading("c", 42.2),
    ]

    let hot = hotSensors(readings, 30.0)
    emit("sensors.hot.count", hot |> count)

    match parseConfig(["host": "localhost", "port": "8080"]) {
        Ok(cfg)  => emit("config.port", cfg.port)
        Err(msg) => emit("config.error", msg)
    }
}
