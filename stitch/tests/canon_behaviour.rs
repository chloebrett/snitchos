//! What the canon programs actually *do*.
//!
//! `canon.rs` gates the whole corpus structurally — everything parses, nothing
//! reports a type error. That is a floor, not a warranty: a wrapping routine
//! that silently drops the last word passes it. These tests pin behaviour for
//! the library modules, which is what makes them safe to import (from another
//! canon program, from the shell) and honest as training data — a corpus whose
//! premise is "correct by construction" has to keep the promise where the
//! construction is a human.
//!
//! Runnable programs (`primes.st`, `double.st`) are exercised where their
//! effects are: the itests, under a booted kernel. These are the pure ones.

use stitch::testing::run_modules;
use stitch::value::Value;

/// Load a canon library alongside a driver module that calls into it.
fn with_text(body: &str) -> Value {
    let text = std::fs::read_to_string(canon_path("fs-image/lib/text.st"))
        .expect("lib/text.st should be readable");
    let driver = format!("use text\nuse Str\n\nmain() = {body}");
    run_modules(&[("text", &text), ("driver", &driver)], "driver")
}

/// Load `lib/stats.st` alongside a driver that calls into it.
fn with_stats(body: &str) -> Value {
    let stats = std::fs::read_to_string(canon_path("fs-image/lib/stats.st"))
        .expect("lib/stats.st should be readable");
    let driver = format!("use stats\n\nmain() = {body}");
    run_modules(&[("stats", &stats), ("driver", &driver)], "driver")
}

fn int_of(value: &Value) -> i64 {
    match value {
        Value::Int(n) => *n,
        other => panic!("expected an Int, got {other:?}"),
    }
}

fn canon_path(relative: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("stitch/ has a parent")
        .join(relative)
}

fn str_of(value: &Value) -> String {
    match value {
        Value::Str(s) => s.to_string(),
        other => panic!("expected a Str, got {other:?}"),
    }
}

#[test]
fn pad_widens_to_the_column_and_never_truncates() {
    assert_eq!(str_of(&with_text(r#"text.pad("ok", 5)"#)), "ok   ");
    assert_eq!(str_of(&with_text(r#"text.padLeft("7", 3)"#)), "  7");
    assert_eq!(
        str_of(&with_text(r#"text.pad("overlong", 3)"#)),
        "overlong",
        "a value wider than its column must look wrong, not be cut"
    );
}

#[test]
fn wrap_breaks_at_spaces_and_keeps_every_word() {
    let wrapped = with_text(r#"Str.join(text.wrap("the quick brown fox", 9), "|")"#);

    assert_eq!(str_of(&wrapped), "the quick|brown fox");
}

/// The case a naive `wrap` gets wrong: a single word longer than the width has
/// nowhere to break, and cutting it would corrupt an identifier.
#[test]
fn wrap_gives_an_overlong_word_its_own_line_intact() {
    let wrapped = with_text(r#"Str.join(text.wrap("a supercalifragilistic b", 5), "|")"#);

    assert_eq!(str_of(&wrapped), "a|supercalifragilistic|b");
}

#[test]
fn indent_prefixes_content_lines_and_leaves_blank_ones_alone() {
    let indented = with_text(r#"text.indent("one\n\ntwo", 2)"#);

    assert_eq!(str_of(&indented), "  one\n\n  two");
}

#[test]
fn title_case_normalises_the_whole_word() {
    assert_eq!(str_of(&with_text(r#"text.titleCase("BOOT")"#)), "Boot");
    assert_eq!(str_of(&with_text(r#"text.titleCase("")"#)), "");
}

#[test]
fn repeat_of_a_non_positive_count_is_empty_rather_than_an_error() {
    assert_eq!(str_of(&with_text(r#"text.repeatStr("ab", 0)"#)), "");
    assert_eq!(str_of(&with_text(r#"text.repeatStr("ab", -3)"#)), "");
}

#[test]
fn summarise_reports_the_whole_shape_of_a_list() {
    let xs = "[3, 1, 4, 1, 5]";

    assert_eq!(int_of(&with_stats(&format!("stats.summarise({xs}).count"))), 5);
    assert_eq!(int_of(&with_stats(&format!("stats.summarise({xs}).total"))), 14);
    assert_eq!(int_of(&with_stats(&format!("stats.summarise({xs}).lowest"))), 1);
    assert_eq!(int_of(&with_stats(&format!("stats.summarise({xs}).highest"))), 5);
    assert_eq!(int_of(&with_stats(&format!("stats.range(stats.summarise({xs}))"))), 4);
}

/// The median has to sort first — reading the middle of the *input* returns
/// whatever happened to be there, which is right often enough to hide the bug.
#[test]
fn median_sorts_before_it_picks_the_middle() {
    assert_eq!(int_of(&with_stats("stats.summarise([5, 1, 3]).median")), 3);
    assert_eq!(int_of(&with_stats("stats.summarise([9, 1, 2, 8]).median")), 5);
}

/// Truncation is the documented contract, so it is asserted rather than left to
/// be discovered: 14/5 is 2, not 3.
#[test]
fn mean_truncates_rather_than_rounding() {
    assert_eq!(int_of(&with_stats("stats.summarise([3, 1, 4, 1, 5]).mean")), 2);
}

/// The empty list is the case a summariser gets wrong by dividing by zero or by
/// forcing every caller through a `Maybe`. Zeroes are the true answer.
#[test]
fn the_empty_list_summarises_to_zeroes() {
    assert_eq!(int_of(&with_stats("stats.summarise([]).count")), 0);
    assert_eq!(int_of(&with_stats("stats.summarise([]).mean")), 0);
    assert_eq!(int_of(&with_stats("stats.range(stats.summarise([]))")), 0);
}

/// One reading has no spread, and its median, mean, and both extremes are all
/// that reading — the degenerate case every branch has to agree on.
#[test]
fn a_single_reading_is_its_own_summary() {
    assert_eq!(int_of(&with_stats("stats.summarise([7]).median")), 7);
    assert_eq!(int_of(&with_stats("stats.summarise([7]).mean")), 7);
    assert_eq!(int_of(&with_stats("stats.range(stats.summarise([7]))")), 0);
}
