//! Integration tests for the AST→source printer.
//!
//! The printer's contract is a *round-trip*, not a byte format: printing an AST
//! and re-parsing it must yield an equal AST. That is the property the corpus
//! pipeline depends on — babble output parses by construction, so re-printing it
//! is how flat, space-padded token streams become laid-out Stitch without
//! trusting the printer's taste.
//!
//! `Expr`/`Effect` compare on shape, not span, so a round-tripped AST is equal
//! to its origin even though every position has moved.

use std::path::{Path, PathBuf};

use stitch::ast::Item;
use stitch::parser::parse_program;
use stitch::print::print_program;

fn prog(src: &str) -> Vec<Item> {
    parse_program(src).expect("test program should parse")
}

/// The printer's whole contract, on the smallest program that has a body.
fn assert_round_trips(src: &str) {
    let ast = prog(src);
    let printed = print_program(&ast);
    let reparsed = parse_program(&printed)
        .unwrap_or_else(|e| panic!("printed source should re-parse, got {e:?}\n---\n{printed}\n---"));
    assert_eq!(ast, reparsed, "round-trip changed the AST\n---\n{printed}\n---");
}

#[test]
fn round_trips_a_constant() {
    assert_round_trips("let answer = 42");
}

/// A `test` declaration is a shipped item like any other, so it owes the same
/// round-trip. It is also the one item whose name is a string literal, which is
/// the part a printer is most likely to get wrong (quotes, escapes, and a name
/// that must not be re-lexed as an identifier).
#[test]
fn round_trips_a_test_declaration() {
    assert_round_trips(r#"test "adds two numbers" { 1 + 1 }"#);
    assert_round_trips(r#"test "quotes \"inside\" the name" { 1 }"#);
    assert_round_trips(r#"test "emits a span" uses Telemetry { 1 }"#);
}

/// `expect` takes its operand at the loosest binding power, so the printer must
/// not parenthesize a comparison the parser swallowed whole — and must not lose
/// the parens where they *are* load-bearing.
#[test]
fn round_trips_expect() {
    assert_round_trips(r#"test "compares" { expect 1 + 1 == 2 }"#);
    assert_round_trips(r#"test "asserts a bool" { expect true }"#);
    assert_round_trips(r#"test "two in a row" { expect 1 == 1  expect 2 == 2 }"#);
}

#[test]
fn round_trips_literals() {
    assert_round_trips("let i = 42  let f = 3.5  let t = true  let f2 = false  let neg = -7");
}

#[test]
fn round_trips_strings() {
    assert_round_trips(r#"let plain = "hello"  let escapes = "a\"b\nc\\d"  let empty = """#);
}

#[test]
fn round_trips_string_interpolation() {
    assert_round_trips(r#"let greeting = "hi {name}, you are {age + 1}""#);
}

/// Precedence is the printer's one genuinely hard problem: the AST records
/// nesting, the source records operators, and a naive printer flattens
/// `(a + b) * c` into `a + b * c`. Each case here nests *against* the natural
/// precedence, so an unparenthesized print re-parses to a different tree.
#[test]
fn round_trips_operators_against_precedence() {
    assert_round_trips("let a = (1 + 2) * 3");
    assert_round_trips("let b = 1 - (2 - 3)");
    assert_round_trips("let c = (a or b) and c");
    assert_round_trips("let d = not (a and b)");
    assert_round_trips("let e = -(a + b)");
    assert_round_trips("let f = (a |> g) + 1");
}

/// The natural nesting must *not* pick up parentheses — otherwise the printer
/// is correct but emits a distribution of over-parenthesized code that real
/// Stitch never contains, which is worse than useless as training data.
#[test]
fn leaves_natural_precedence_unparenthesized() {
    let ast = prog("let x = 1 + 2 * 3 - 4");
    assert_eq!(print_program(&ast).trim(), "let x = 1 + 2 * 3 - 4");
}

#[test]
fn round_trips_comparisons_and_ranges() {
    assert_round_trips("let a = x < 1  let b = x >= 1  let c = x != 1  let d = 1..10  let e = 1..=10");
}

#[test]
fn round_trips_calls_and_access() {
    assert_round_trips("let a = f(1, 2)  let b = g(label: 1)  let c = p.x  let d = p?.x  let e = xs[0]  let f2 = h()");
}

/// `..` at the head of a call argument is the *spread* (`Point(..p, x: 1)`),
/// not a range, so a range argument has to say so with parentheses. The two
/// share a leading token and nothing else.
#[test]
fn round_trips_a_range_argument_against_the_spread() {
    assert_round_trips("let a = f((..))");
    assert_round_trips("let b = f((..10))");
    assert_round_trips("let c = f(1..10)");
    assert_round_trips("let d = Point(..p, x: 1)");
}

#[test]
fn round_trips_collections() {
    assert_round_trips("let a = [1, 2, 3]  let b = [:]  let c = [k: 1, j: 2]  let d = (1, 2)  let e = ()");
}

#[test]
fn round_trips_lambdas_and_pipelines() {
    assert_round_trips("let a = x -> x + 1  let b = (p, q) -> p * q  let c = () -> 0");
    assert_round_trips("let d = xs |> map($ + 1) |> filter($.ok)");
}

#[test]
fn round_trips_conditionals() {
    assert_round_trips("let a = x > 0 => 1 | 0");
}

#[test]
fn round_trips_blocks_and_statements() {
    assert_round_trips("f() = { let a = 1  let mut b = 2  b = 3  g(b)  a + b }");
    assert_round_trips("g() = { h() }");
    assert_round_trips("empty() = { }");
}

/// Statements are separated by whitespace and terminated by nothing, so a
/// statement ending in an unfinished token reaches into the next one: an open
/// range takes it as the range's end, and a bare `@` takes a following
/// identifier as a field (`@name` is receiver-field shorthand).
///
/// The repair has to know that a statement is not always an expression —
/// `(let label = ..=)` is a syntax error, not a parenthesised binding — so what
/// gets wrapped is the trailing expression, never the statement.
#[test]
fn round_trips_statements_that_would_run_into_the_next_one() {
    assert_round_trips(r#"f() = { let label = (..=) "value" }"#);
    assert_round_trips("g() = { let n = (1..) done() }");
    assert_round_trips("h() = { x == (@) line }");
    assert_round_trips("i() = { @ @ }");
}

/// The repair must be the *smallest* subexpression that ends in the offending
/// token, not the whole statement. Wrapping the statement moves a `(` to its
/// front, where the statement before it reads the parentheses as a call and
/// swallows it — trading one seam for another one line up.
#[test]
fn sealing_a_statement_does_not_hand_the_previous_one_a_call() {
    assert_round_trips(r#"f() = { ["field"] 1.5 ~> (@) token }"#);
    assert_round_trips("g() = { [1] 2 + (3..) done() }");
}

#[test]
fn round_trips_use_callback_sugar() {
    assert_round_trips("f() = { use line <- each(lines)  emit(line) }");
    assert_round_trips("g() = { use <- defer()  0 }");
}

#[test]
fn round_trips_match() {
    assert_round_trips("f(s) = match s { Circle(r) => r  Rect(w, h) => w * h }");
    assert_round_trips("g(x) = match x { 0 => \"zero\"  n if n > 0 => \"pos\"  _ => \"neg\" }");
    assert_round_trips("h(p) = match p { (a, b) => a + b }");
    assert_round_trips("i(x) = match x { 1 | 2 | 3 => true  _ => false }");
    assert_round_trips("j(x) = match x { true => 1  1.5 => 2  \"s\" => 3  _ => 4 }");
}

#[test]
fn round_trips_subjectless_match() {
    assert_round_trips("f(x) = match { x > 10 => \"big\"  x > 0 => \"small\"  _ => \"none\" }");
}

#[test]
fn round_trips_effect_forms() {
    assert_round_trips("f() = handle emit with (m) -> log(m) { work() }");
    assert_round_trips("g() = without Telemetry { work() }");
}

#[test]
fn round_trips_declarations() {
    assert_round_trips("prod Point(x: Int, y: Int)");
    assert_round_trips("prod Reading(sensor: Str, mut celsius: Float)");
    assert_round_trips("ext prod Box<T>(ext value: T)");
    assert_round_trips("sum Maybe<T> = Some(T) | None");
    assert_round_trips("ext sum Shape = Circle(radius: Float) | Rect(w: Float, h: Float)");
    assert_round_trips("use Str");
    assert_round_trips("use List.{map, filter}");
}

#[test]
fn round_trips_functions() {
    assert_round_trips("double(n) = n * 2");
    assert_round_trips("ext area(s: Shape) -> Float = 0.0");
    assert_round_trips("log(m: Str) uses Telemetry = emit(m)");
    assert_round_trips("both(m) uses Telemetry, Console = emit(m)");
}

#[test]
fn round_trips_types() {
    assert_round_trips("f(a: List<Int>, b: Result<T, E>, c: (Int, Str), d: ()) -> Int -> Str = g");
    assert_round_trips("h(f: (Int, Int) -> Int) = f");
}

/// Every `.st` file the repo ships — the real corpus. A printer that round-trips
/// hand-written constructs but not the programs we actually run is not usable
/// for corpus work, and this is the only test that knows the difference.
#[test]
fn round_trips_every_shipped_program() {
    let sources = shipped_stitch_sources();
    assert!(
        sources.len() >= 4,
        "expected the shipped .st corpus to be found, got {sources:?}"
    );
    for path in sources {
        let src = std::fs::read_to_string(&path).expect("shipped source should be readable");
        let ast = parse_program(&src)
            .unwrap_or_else(|e| panic!("{} should parse: {e:?}", path.display()));
        let printed = print_program(&ast);
        let reparsed = parse_program(&printed).unwrap_or_else(|e| {
            panic!("printed {} should re-parse: {e:?}\n---\n{printed}\n---", path.display())
        });
        assert_eq!(ast, reparsed, "round-trip changed the AST of {}", path.display());
    }
}

/// The `.st` files under the repo's `fs-image/` (what ships to ramfs) plus the
/// interpreter's own prelude.
fn shipped_stitch_sources() -> Vec<PathBuf> {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("stitch/ has a parent")
        .to_path_buf();
    let mut found = vec![repo.join("stitch/src/prelude.st")];
    collect_st(&repo.join("fs-image"), &mut found);
    found.sort();
    found
}

fn collect_st(dir: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_st(&path, found);
        } else if path.extension().is_some_and(|e| e == "st") {
            found.push(path);
        }
    }
}

#[test]
fn round_trips_contracts_and_impls() {
    assert_round_trips("contract Show { show() -> Str }");
    assert_round_trips("contract Default<T> { free make() -> @  mut reset()  show() -> Str = \"x\" }");
    assert_round_trips("on Point { len() -> Float = 0.0 }");
    assert_round_trips("on Point : Show { show() -> Str = \"p\"  mut move() = { } }");
    assert_round_trips("on Point { grow() uses Telemetry = @ }");
}
