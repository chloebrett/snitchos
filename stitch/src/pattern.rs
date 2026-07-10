//! `match` evaluation and pattern matching. A pattern either binds (extending
//! the environment) or fails; `eval_match` walks the arms in order and runs the
//! first whose pattern matches and whose guard holds.

use crate::ast::Pattern;
use crate::core_ir::CoreMatchArm;
use crate::env::Env;
use crate::interp::{eval, eval_tail};
use crate::ops::as_bool;
use crate::value::{RuntimeError, Value};

/// Evaluate a `match`: try each arm's pattern against `subject` in order; the
/// first that matches (and whose guard, if any, holds) wins. No arm matching is
/// a runtime error (v0 has no static exhaustiveness check yet).
pub(crate) fn eval_match(
    subject: &Value,
    arms: &[CoreMatchArm],
    env: &Env,
    tail: bool,
) -> Result<Value, RuntimeError> {
    for arm in arms {
        let Some(bound) = try_match(&arm.pattern, subject, env) else {
            continue;
        };
        let guard_holds = match &arm.guard {
            Some(guard) => as_bool(&eval(guard, &bound)?, "match guard")?,
            None => true,
        };
        if guard_holds {
            return if tail { eval_tail(&arm.body, &bound) } else { eval(&arm.body, &bound) };
        }
    }
    Err(RuntimeError::new("no match arm matched"))
}

/// Try to match `pattern` against `value`. On success, return `env` extended
/// with the pattern's bindings; on failure, `None`.
fn try_match(pattern: &Pattern, value: &Value, env: &Env) -> Option<Env> {
    match pattern {
        Pattern::Wildcard => Some(env.clone()),
        Pattern::Binding(name) => Some(env.extend(name.clone(), value.clone())),
        Pattern::Int(n) => (*value == Value::Int(*n)).then(|| env.clone()),
        Pattern::Float(f) => (*value == Value::Float(*f)).then(|| env.clone()),
        Pattern::Bool(b) => (*value == Value::Bool(*b)).then(|| env.clone()),
        // Match a `prod`/variant by name and arity, then recurse into each
        // field, threading the bindings through (a sub-match feeds the next).
        Pattern::Constructor { name, args } => {
            let Value::Data(data) = value else {
                return None;
            };
            if data.variant != *name || data.fields.len() != args.len() {
                return None;
            }
            let mut bound = env.clone();
            for (subpattern, (_, field)) in args.iter().zip(&data.fields) {
                bound = try_match(subpattern, field, &bound)?;
            }
            Some(bound)
        }
        // First alternative that matches wins, with its bindings.
        Pattern::Or(alternatives) => alternatives
            .iter()
            .find_map(|alternative| try_match(alternative, value, env)),
        Pattern::Str(text) => match value {
            Value::Str(s) => (s.as_ref() == text).then(|| env.clone()),
            _ => None,
        },
        // `()` pattern matches unit; `(a, b, …)` matches a tuple element-wise.
        Pattern::Tuple(subpatterns) if subpatterns.is_empty() => {
            matches!(value, Value::Unit).then(|| env.clone())
        }
        Pattern::Tuple(subpatterns) => {
            let Value::Tuple(elements) = value else {
                return None;
            };
            if elements.len() != subpatterns.len() {
                return None;
            }
            let mut bound = env.clone();
            for (subpattern, element) in subpatterns.iter().zip(elements.iter()) {
                bound = try_match(subpattern, element, &bound)?;
            }
            Some(bound)
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::testing::{run, run_err, run_program};
    use crate::value::Value;

    #[test]
    fn matches_a_literal_arm_else_the_wildcard() {
        assert_eq!(run("match 0 { 0 => 1  _ => 2 }"), Value::Int(1));
        assert_eq!(run("match 5 { 0 => 1  _ => 2 }"), Value::Int(2));
    }

    #[test]
    fn a_binding_pattern_binds_the_subject() {
        assert_eq!(run("match 7 { x => x + 1 }"), Value::Int(8));
    }

    #[test]
    fn a_guard_can_reject_an_arm_and_fall_through() {
        assert_eq!(run("match 5 { x if x > 10 => 1  _ => 2 }"), Value::Int(2));
        assert_eq!(run("match 5 { x if x > 0 => 1  _ => 2 }"), Value::Int(1));
    }

    #[test]
    fn no_matching_arm_is_an_error() {
        assert_eq!(
            run_err("match 5 { 0 => 1  1 => 2 }"),
            "no match arm matched"
        );
    }

    #[test]
    fn matches_and_destructures_a_sum_variant() {
        assert_eq!(
            run_program(
                "sum Opt = Just(Int) | Nothing  main() = match Just(5) { Just(x) => x  Nothing => 0 }"
            ),
            Value::Int(5)
        );
        assert_eq!(
            run_program(
                "sum Opt = Just(Int) | Nothing  main() = match Nothing { Just(x) => x  Nothing => 0 }"
            ),
            Value::Int(0)
        );
    }

    #[test]
    fn destructures_a_named_field_variant_positionally() {
        assert_eq!(
            run_program(
                "sum Shape = Circle(radius: Int) | Rect(w: Int, h: Int)  \
                 main() = match Rect(3, 4) { Circle(r) => r  Rect(w, h) => w * h }"
            ),
            Value::Int(12)
        );
    }

    #[test]
    fn a_constructor_pattern_does_not_match_a_different_value() {
        assert_eq!(
            run_program(
                "sum Opt = Just(Int) | Nothing  main() = match 5 { Just(x) => x  _ => 99 }"
            ),
            Value::Int(99)
        );
    }

    #[test]
    fn nested_constructor_patterns_destructure_deeply() {
        assert_eq!(
            run_program(
                "sum Opt = Just(Int) | Nothing  main() = match Just(Just(7)) { Just(Just(n)) => n  _ => 0 }"
            ),
            Value::Int(7)
        );
    }

    #[test]
    fn matches_an_or_pattern_of_literals() {
        assert_eq!(run("match 2 { 1 | 2 | 3 => 100  _ => 0 }"), Value::Int(100));
        assert_eq!(run("match 9 { 1 | 2 | 3 => 100  _ => 0 }"), Value::Int(0));
    }

    #[test]
    fn matches_an_or_pattern_of_variants() {
        assert_eq!(
            run_program(
                "sum Color = Red | Green | Blue  main() = match Green { Red | Green => 1  Blue => 2 }"
            ),
            Value::Int(1)
        );
    }

    #[test]
    fn matches_a_string_literal_pattern() {
        assert_eq!(run(r#"match "hi" { "hi" => 1  _ => 0 }"#), Value::Int(1));
        assert_eq!(run(r#"match "yo" { "hi" => 1  _ => 0 }"#), Value::Int(0));
    }

    #[test]
    fn destructures_a_tuple_pattern() {
        assert_eq!(run("match (1, 2) { (a, b) => a + b }"), Value::Int(3));
    }

    #[test]
    fn destructures_a_nested_tuple_pattern() {
        assert_eq!(
            run("match ((1, 2), 3) { ((a, b), c) => a + b + c }"),
            Value::Int(6)
        );
    }

    #[test]
    fn a_tuple_pattern_arity_must_match() {
        assert_eq!(
            run("match (1, 2, 3) { (a, b) => 0  _ => 99 }"),
            Value::Int(99)
        );
    }

    #[test]
    fn the_empty_tuple_pattern_matches_unit() {
        assert_eq!(run("match () { () => 42 }"), Value::Int(42));
    }
}
