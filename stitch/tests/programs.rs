//! Integration tests for `parse_program` — top-level declarations.

use stitch::ast::Item;
use stitch::parser::parse_program;

fn prog(src: &str) -> Vec<Item> {
    parse_program(src).expect("integration test program should parse")
}

#[test]
fn std_sum_types() {
    // The three std error/absence types from the design docs, parsed together.
    insta::assert_debug_snapshot!(prog(
        "sum Maybe<T> = Some(T) | None  \
         sum Result<T, E> = Ok(T) | Err(E)  \
         sum Either<A, B> = Left(A) | Right(B)"
    ));
}

#[test]
fn product_and_sum_together() {
    insta::assert_debug_snapshot!(prog(
        "prod Reading(sensor: Str, mut celsius: Float)  \
         sum Shape = Circle(radius: Float) | Rect(w: Float, h: Float)"
    ));
}

#[test]
fn a_small_program() {
    // Types + functions together: a sum, a match-based function, and a
    // pipeline function with placeholders — most of a real Stitch module.
    insta::assert_debug_snapshot!(prog(
        "sum Shape = Circle(radius: Float) | Rect(w: Float, h: Float)  \
         area(s) = match s { Circle(r) => 3 * r * r  Rect(w, h) => w * h }  \
         hot(readings, threshold) = readings |> filter($.celsius > threshold) |> map($.sensor)"
    ));
}
