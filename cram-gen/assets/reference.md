# Stitch reference

// Modules & visibility
use List                                 // import a module
use text.{pad, wrap}                     // import named items
ext foo(...)                             // exported; without `ext`, private

// Exporting a type: `ext` goes BEFORE `prod`/`sum`, and every field that
// should be visible needs its own `ext` too.
ext prod Summary(ext count: Int, ext total: Int)
ext sum Shape = Circle(Int) | Square(Int)
// `ext` is NOT valid on `contract`, on an `on` block, or on `use`.

// Data — fields are immutable unless marked `mut`
prod Point(x: Int, y: Int)
prod Counter(mut n: Int)
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

// Conditionals. `match` has two forms: guards, and patterns over a value.
cond => thenValue | elseValue
match { n == 0 => "zero"   n > 0 => "positive"   _ => "negative" }
match m { Some(v) => v   None => 0 }

// There is no `if`/`then`/`else` anywhere, including inside a lambda:
xs |> map(x -> x > 0 => "pos" | "neg")            // right
fold(xs, None, (acc, x) -> match acc { Some(_) => acc   None => Some(x) })

// Tuple patterns work in `match`, NOT in a `let` binding:
match pair { (a, b) => a + b }                    // right
// let (a, b) = pair                              // does NOT parse

// Booleans are words, not symbols
a and b                                  // NOT &&
a or b                                   // NOT ||
n % d != 0

// Field access on a value is a plain dot. `@` is ONLY the receiver, and only
// inside an `on` block — never on a lambda parameter or a local.
b.start                                  // a field of `b`
@start                                   // field of the receiver, inside `on`

// Methods attach with `on`; `@` is the receiver
on Counter {
    bumped() -> Counter = Counter(n: @n + 1)
}
on Counter : Drawable {                  // `: Contract` declares conformance
    draw() uses Canvas = renderBar(@n)
}

// Capabilities are declared on the signature
report(xs: List<Int>) uses Telemetry = ...

// Short-circuit family
value?                                   // unwrap a Result/Maybe or short-circuit
user?.address                            // safe navigation

// Ranges and lists
[1, 2, 3]
2..n                                     // exclusive
1..=n                                    // inclusive

// Conversions are free functions, not methods
toStr(n)                                 // Int -> Str, NOT n.toString()
Str.parseInt(s)                          // Str -> Maybe<Int>

// Pipes and lambdas. A pipe supplies the FIRST argument, so these are identical:
fold(xs, 0, (acc, x) -> acc + x)
xs |> fold(0, (acc, x) -> acc + x)
xs |> map(x -> x * 2) |> filter(x -> x > 3)
xs |> map($.name)                        // `$a`/`$b` are positional placeholders

// Tests are ordinary items
test "double doubles" { expect double(2) == 4 }

# Built-in functions

These are implemented natively and are always available. The prelude below is
written on top of them. Argument order matches the examples: a pipe supplies the
first argument, so `xs |> map(f)` is `map(xs, f)`.

// Lists and sequences
map(xs, f)              filter(xs, pred)        fold(xs, init, f)
foldWhile(xs, init, f)  flatMap(xs, f)          concat(xs, ys)
sort(xs)                sortBy(xs, key)         reverse(xs)
take(xs, n)             takeWhile(xs, pred)     drop(xs, n)
dropWhile(xs, pred)     zip(xs, ys)             enumerate(xs)
toList(xs)              Seq.repeat(x)           Seq.iterate(x, f)

// Indexed list access — module-qualified, the bare names do not resolve
List.at(xs, i)          List.set(xs, i, v)
List.insert(xs, i, v)   List.removeAt(xs, i)

// Strings
Str.length(s)           Str.slice(s, from, to)  Str.split(s, sep)
Str.join(xs, sep)       Str.upper(s)            Str.lower(s)
Str.trim(s)             Str.contains(s, sub)    Str.startsWith(s, pre)
Str.replace(s, a, b)    Str.parseInt(s)         // -> Maybe<Int>

// Conversion
toStr(x)                // any -> Str. NOT x.toString()

// Capability-carrying (declare them in the `uses` row)
print(s)                readLine()              readByte()
readFile(path)          fsWrite(path, s)        writeConsole(s)
emit(name, value)       span(name, f)
