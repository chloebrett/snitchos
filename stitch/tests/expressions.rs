//! Integration tests: whole-expression snapshots over the public parser API.
//! These exercise many features at once — operators, precedence, pipes,
//! ranges, calls, field access, indexing — the way real Stitch code combines
//! them. (Lambdas, `match`, strings, and collection literals arrive later.)

use stitch::ast::Expr;
use stitch::parser::parse;

fn p(src: &str) -> Expr {
    parse(src).expect("integration test input should parse")
}

#[test]
fn sensor_pipeline() {
    insta::assert_debug_snapshot!(p("readings |> filter(hot) |> map(celsius) |> total"));
}

#[test]
fn arithmetic_and_logic() {
    insta::assert_debug_snapshot!(p("a + b * 2 - c / d < limit and not done"));
}

#[test]
fn postfix_chain() {
    insta::assert_debug_snapshot!(p("cache[key]?.value + 1"));
}

#[test]
fn nested_calls() {
    insta::assert_debug_snapshot!(p("f(g(x), h(y, z))"));
}

#[test]
fn range_fold() {
    insta::assert_debug_snapshot!(p("0 .. count |> map(square) |> fold(0, add)"));
}

#[test]
fn pipeline_with_lambdas() {
    insta::assert_debug_snapshot!(p(
        "readings |> filter(r -> r.celsius > 30) |> map(r -> r.celsius)"
    ));
}
