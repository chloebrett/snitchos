//! The built-in (native) functions — the standard-library combinators and host
//! I/O that can't be expressed in Stitch itself (`map`/`filter`/`fold`/`join`,
//! plus the telemetry stubs `emit`/`span`). Everything else in the stdlib is the
//! Stitch-source prelude, layered on top of these.

use crate::env::Env;
use crate::interp::apply_values;
use crate::value::{LazySeq, NativeFn, RuntimeError, Step, TelemetryEvent, Value};

/// The native functions, registered into every program's globals.
pub(crate) const NATIVES: &[NativeFn] = &[
    NativeFn {
        name: "map",
        arity: 2,
        func: native_map,
    },
    NativeFn {
        name: "filter",
        arity: 2,
        func: native_filter,
    },
    NativeFn {
        name: "fold",
        arity: 3,
        func: native_fold,
    },
    NativeFn {
        name: "join",
        arity: 2,
        func: native_join,
    },
    NativeFn {
        name: "emit",
        arity: 2,
        func: native_emit,
    },
    NativeFn {
        name: "span",
        arity: 2,
        func: native_span,
    },
    NativeFn {
        name: "toList",
        arity: 1,
        func: native_to_list,
    },
    NativeFn {
        name: "take",
        arity: 2,
        func: native_take,
    },
    NativeFn {
        name: "iterate",
        arity: 2,
        func: native_iterate,
    },
    NativeFn {
        name: "repeat",
        arity: 1,
        func: native_repeat,
    },
    NativeFn {
        name: "takeWhile",
        arity: 2,
        func: native_take_while,
    },
    NativeFn {
        name: "foldWhile",
        arity: 3,
        func: native_fold_while,
    },
    // --- string operations (exposed under the `Str` module; `str`-prefixed
    //     internally so generic names don't clutter the flat namespace) ---
    NativeFn {
        name: "strUpper",
        arity: 1,
        func: native_upper,
    },
    NativeFn {
        name: "strLower",
        arity: 1,
        func: native_lower,
    },
    NativeFn {
        name: "strLength",
        arity: 1,
        func: native_length,
    },
    NativeFn {
        name: "strTrim",
        arity: 1,
        func: native_trim,
    },
    NativeFn {
        name: "strContains",
        arity: 2,
        func: native_str_contains,
    },
    NativeFn {
        name: "strStartsWith",
        arity: 2,
        func: native_starts_with,
    },
    NativeFn {
        name: "strSplit",
        arity: 2,
        func: native_split,
    },
    NativeFn {
        name: "strReplace",
        arity: 3,
        func: native_replace,
    },
];

/// `foldWhile(coll, init, f)` — reduce left-to-right with an early stop. `f(acc,
/// elem)` returns `Some(newAcc)` to continue or `None` to stop (the result is
/// the accumulator from *before* the stopping step). The accumulator-aware
/// terminator: unlike `takeWhile`, the stop decision can depend on `acc`, so it
/// can consume an infinite sequence. Works on a `List` or a `Seq`.
fn native_fold_while(args: &[Value], env: &Env) -> Result<Value, RuntimeError> {
    let [collection, init, function] = args else {
        return Err(RuntimeError::new("foldWhile expects (collection, init, function)"));
    };
    let mut acc = init.clone();
    if let Value::Seq(_) = collection {
        let mut current = collection.clone();
        while let Step::Cons(head, tail) = force_seq(&current)? {
            match fold_while_step(function, &acc, &head, env)? {
                Some(next) => acc = next,
                None => return Ok(acc),
            }
            current = tail;
        }
        return Ok(acc);
    }
    for item in expect_list("foldWhile", collection)? {
        match fold_while_step(function, &acc, item, env)? {
            Some(next) => acc = next,
            None => return Ok(acc),
        }
    }
    Ok(acc)
}

/// Run one `foldWhile` step: `Some(acc)` to continue, `None` to stop.
fn fold_while_step(
    f: &Value,
    acc: &Value,
    elem: &Value,
    env: &Env,
) -> Result<Option<Value>, RuntimeError> {
    match apply_values(f, &[acc.clone(), elem.clone()], env)? {
        Value::Data(d) if d.type_name == "Maybe" && d.variant == "Some" => {
            Ok(Some(d.fields.first().map_or(Value::Unit, |(_, v)| v.clone())))
        }
        Value::Data(d) if d.type_name == "Maybe" && d.variant == "None" => Ok(None),
        other => Err(RuntimeError::new(format!(
            "foldWhile step must return a Maybe (Some to continue, None to stop), got {}",
            other.kind()
        ))),
    }
}

/// Force a value that must be a `Seq` to its next step.
fn force_seq(value: &Value) -> Result<Step, RuntimeError> {
    match value {
        Value::Seq(lazy) => lazy.force(),
        other => Err(RuntimeError::new(format!(
            "expected a Seq, got {}",
            other.kind()
        ))),
    }
}

/// `iterate(seed, f)` — the infinite `Seq` `seed, f(seed), f(f(seed)), …`.
fn native_iterate(args: &[Value], env: &Env) -> Result<Value, RuntimeError> {
    let [seed, f] = args else {
        return Err(RuntimeError::new("iterate expects (seed, function)"));
    };
    Ok(iterate_seq(seed.clone(), f.clone(), env.clone()))
}

fn iterate_seq(current: Value, f: Value, env: Env) -> Value {
    Value::Seq(LazySeq::new(move || {
        // The head is `current`; the tail defers `f(current)` until it is forced,
        // so `f` runs once per element actually demanded — never one ahead.
        let (f, env, current_for_tail) = (f.clone(), env.clone(), current.clone());
        let tail = Value::Seq(LazySeq::new(move || {
            let next = apply_values(&f, std::slice::from_ref(&current_for_tail), &env)?;
            force_seq(&iterate_seq(next, f.clone(), env.clone()))
        }));
        Ok(Step::Cons(current.clone(), tail))
    }))
}

/// `repeat(x)` — the infinite `Seq` `x, x, x, …`.
fn native_repeat(args: &[Value], _env: &Env) -> Result<Value, RuntimeError> {
    let [x] = args else {
        return Err(RuntimeError::new("repeat expects (value)"));
    };
    Ok(repeat_seq(x.clone()))
}

fn repeat_seq(x: Value) -> Value {
    Value::Seq(LazySeq::new(move || {
        Ok(Step::Cons(x.clone(), repeat_seq(x.clone())))
    }))
}

/// `take(n, seq)` — a lazy `Seq` of at most the first `n` elements of `seq`.
/// Lazy, so it works on an infinite sequence.
fn native_take(args: &[Value], _env: &Env) -> Result<Value, RuntimeError> {
    let [count, seq] = args else {
        return Err(RuntimeError::new("take expects (count, seq)"));
    };
    let Value::Int(count) = count else {
        return Err(RuntimeError::new(format!(
            "take count must be an Int, got {}",
            count.kind()
        )));
    };
    if !matches!(seq, Value::Seq(_)) {
        return Err(RuntimeError::new(format!(
            "take expects a Seq, got {}",
            seq.kind()
        )));
    }
    Ok(take_seq(*count, seq.clone()))
}

/// A lazy `Seq` yielding at most `n` elements of `seq`. Each forced step takes
/// one element and defers the rest (with `n - 1`).
fn take_seq(n: i64, seq: Value) -> Value {
    Value::Seq(LazySeq::new(move || {
        if n <= 0 {
            return Ok(Step::Nil);
        }
        let Value::Seq(lazy) = &seq else {
            // `seq` is always a Seq: validated by `take`, and forced tails are
            // Seqs by construction.
            return Ok(Step::Nil);
        };
        match lazy.force()? {
            Step::Nil => Ok(Step::Nil),
            Step::Cons(head, tail) => Ok(Step::Cons(head, take_seq(n - 1, tail))),
        }
    }))
}

/// `toList(seq)` — drain a lazy `Seq` into an eager `List` by forcing it to the
/// end. Diverges on an infinite sequence (force it through `take` first).
fn native_to_list(args: &[Value], _env: &Env) -> Result<Value, RuntimeError> {
    let [seq] = args else {
        return Err(RuntimeError::new("toList expects (seq)"));
    };
    let mut items = Vec::new();
    let mut current = seq.clone();
    loop {
        let Value::Seq(lazy) = &current else {
            return Err(RuntimeError::new(format!(
                "toList expects a Seq, got {}",
                current.kind()
            )));
        };
        match lazy.force()? {
            Step::Nil => break,
            Step::Cons(head, tail) => {
                items.push(head);
                current = tail;
            }
        }
    }
    Ok(Value::List(items.into()))
}

/// `span(name, body)` — open a span, run the zero-argument `body` thunk, close
/// the span, and return the body's value. The `use <- span(name)` sugar makes
/// the rest of a block the body. v0 stub for span frames on the wire.
fn native_span(args: &[Value], env: &Env) -> Result<Value, RuntimeError> {
    let [name, body] = args else {
        return Err(RuntimeError::new("span expects (name, body)"));
    };
    let Value::Str(name) = name else {
        return Err(RuntimeError::new(format!(
            "span name must be a Str, got {}",
            name.kind()
        )));
    };
    env.emit(TelemetryEvent::SpanOpen {
        name: name.to_string(),
    });
    let result = apply_values(body, &[], env)?;
    env.emit(TelemetryEvent::SpanClose {
        name: name.to_string(),
    });
    Ok(result)
}

/// `emit(name, value)` — record a metric sample. v0 stub for the wire protocol.
fn native_emit(args: &[Value], env: &Env) -> Result<Value, RuntimeError> {
    let [name, value] = args else {
        return Err(RuntimeError::new("emit expects (name, value)"));
    };
    let Value::Str(name) = name else {
        return Err(RuntimeError::new(format!(
            "emit name must be a Str, got {}",
            name.kind()
        )));
    };
    env.emit(TelemetryEvent::Emit {
        name: name.to_string(),
        value: value.clone(),
    });
    Ok(Value::Unit)
}

/// Require a list argument, with an error tagged by the combinator `name`.
/// Extract the `&str` from a `Value::Str`, or a typed error naming the builtin.
fn expect_str<'a>(name: &str, value: &'a Value) -> Result<&'a str, RuntimeError> {
    match value {
        Value::Str(text) => Ok(text),
        other => Err(RuntimeError::new(format!(
            "{name} expects a Str, got {}",
            other.kind()
        ))),
    }
}

/// `Str.upper(s)` — `s` uppercased.
fn native_upper(args: &[Value], _env: &Env) -> Result<Value, RuntimeError> {
    let [s] = args else {
        return Err(RuntimeError::new("Str.upper expects (s)"));
    };
    Ok(Value::Str(expect_str("Str.upper", s)?.to_uppercase().into()))
}

/// `Str.lower(s)` — `s` lowercased.
fn native_lower(args: &[Value], _env: &Env) -> Result<Value, RuntimeError> {
    let [s] = args else {
        return Err(RuntimeError::new("Str.lower expects (s)"));
    };
    Ok(Value::Str(expect_str("Str.lower", s)?.to_lowercase().into()))
}

/// `Str.length(s)` — the number of Unicode scalar values (chars) in `s`.
fn native_length(args: &[Value], _env: &Env) -> Result<Value, RuntimeError> {
    let [s] = args else {
        return Err(RuntimeError::new("Str.length expects (s)"));
    };
    let count = expect_str("Str.length", s)?.chars().count();
    Ok(Value::Int(i64::try_from(count).unwrap_or(i64::MAX)))
}

/// `Str.trim(s)` — `s` with surrounding whitespace removed.
fn native_trim(args: &[Value], _env: &Env) -> Result<Value, RuntimeError> {
    let [s] = args else {
        return Err(RuntimeError::new("Str.trim expects (s)"));
    };
    Ok(Value::Str(expect_str("Str.trim", s)?.trim().into()))
}

/// `Str.contains(s, sub)` — whether `sub` occurs in `s` (substring, not element).
fn native_str_contains(args: &[Value], _env: &Env) -> Result<Value, RuntimeError> {
    let [s, sub] = args else {
        return Err(RuntimeError::new("Str.contains expects (s, sub)"));
    };
    let haystack = expect_str("Str.contains", s)?;
    let needle = expect_str("Str.contains", sub)?;
    Ok(Value::Bool(haystack.contains(needle)))
}

/// `Str.startsWith(s, prefix)` — whether `s` begins with `prefix`.
fn native_starts_with(args: &[Value], _env: &Env) -> Result<Value, RuntimeError> {
    let [s, prefix] = args else {
        return Err(RuntimeError::new("Str.startsWith expects (s, prefix)"));
    };
    let text = expect_str("Str.startsWith", s)?;
    let prefix = expect_str("Str.startsWith", prefix)?;
    Ok(Value::Bool(text.starts_with(prefix)))
}

/// `Str.split(s, sep)` — split `s` on each occurrence of `sep` into a `List<Str>`.
fn native_split(args: &[Value], _env: &Env) -> Result<Value, RuntimeError> {
    let [s, sep] = args else {
        return Err(RuntimeError::new("Str.split expects (s, sep)"));
    };
    let text = expect_str("Str.split", s)?;
    let sep = expect_str("Str.split", sep)?;
    let parts = text
        .split(sep)
        .map(|piece| Value::Str(piece.into()))
        .collect::<Vec<_>>();
    Ok(Value::List(parts.into()))
}

/// `Str.replace(s, from, to)` — `s` with every `from` replaced by `to`.
fn native_replace(args: &[Value], _env: &Env) -> Result<Value, RuntimeError> {
    let [s, from, to] = args else {
        return Err(RuntimeError::new("Str.replace expects (s, from, to)"));
    };
    let text = expect_str("Str.replace", s)?;
    let from = expect_str("Str.replace", from)?;
    let to = expect_str("Str.replace", to)?;
    Ok(Value::Str(text.replace(from, to).into()))
}

fn expect_list<'a>(name: &str, value: &'a Value) -> Result<&'a [Value], RuntimeError> {
    match value {
        Value::List(items) => Ok(items),
        other => Err(RuntimeError::new(format!(
            "{name} expects a List, got {}",
            other.kind()
        ))),
    }
}

/// `map(coll, f)` — `f` applied to each element. Polymorphic over the receiver:
/// a `List` maps eagerly to a new `List`; a `Seq` maps lazily to a new `Seq`.
fn native_map(args: &[Value], env: &Env) -> Result<Value, RuntimeError> {
    let [collection, function] = args else {
        return Err(RuntimeError::new("map expects (collection, function)"));
    };
    if let Value::Seq(_) = collection {
        return Ok(map_seq(collection.clone(), function.clone(), env.clone()));
    }
    let mapped = expect_list("map", collection)?
        .iter()
        .map(|item| apply_values(function, std::slice::from_ref(item), env))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Value::List(mapped.into()))
}

/// A lazy `Seq` applying `f` to each element of `seq` on demand.
fn map_seq(seq: Value, f: Value, env: Env) -> Value {
    Value::Seq(LazySeq::new(move || match force_seq(&seq)? {
        Step::Nil => Ok(Step::Nil),
        Step::Cons(head, tail) => {
            let mapped = apply_values(&f, std::slice::from_ref(&head), &env)?;
            Ok(Step::Cons(mapped, map_seq(tail, f.clone(), env.clone())))
        }
    }))
}

/// `filter(coll, pred)` — the elements for which `pred` is true. Eager on a
/// `List`, lazy on a `Seq`.
fn native_filter(args: &[Value], env: &Env) -> Result<Value, RuntimeError> {
    let [collection, predicate] = args else {
        return Err(RuntimeError::new("filter expects (collection, predicate)"));
    };
    if let Value::Seq(_) = collection {
        return Ok(filter_seq(collection.clone(), predicate.clone(), env.clone()));
    }
    let mut kept = Vec::new();
    for item in expect_list("filter", collection)? {
        if keeps(predicate, item, env)? {
            kept.push(item.clone());
        }
    }
    Ok(Value::List(kept.into()))
}

/// A lazy `Seq` of the elements of `seq` for which `pred` holds. Forcing scans
/// past rejected elements until the next match (so it diverges on an infinite
/// sequence with no further matches — inherent to filtering).
fn filter_seq(seq: Value, pred: Value, env: Env) -> Value {
    Value::Seq(LazySeq::new(move || {
        let mut current = seq.clone();
        loop {
            match force_seq(&current)? {
                Step::Nil => return Ok(Step::Nil),
                Step::Cons(head, tail) => {
                    if keeps(&pred, &head, &env)? {
                        return Ok(Step::Cons(head, filter_seq(tail, pred.clone(), env.clone())));
                    }
                    current = tail;
                }
            }
        }
    }))
}

/// Apply a predicate to one element, requiring a `Bool` result. Shared by
/// `filter` and `takeWhile`.
fn keeps(predicate: &Value, item: &Value, env: &Env) -> Result<bool, RuntimeError> {
    match apply_values(predicate, std::slice::from_ref(item), env)? {
        Value::Bool(keep) => Ok(keep),
        other => Err(RuntimeError::new(format!(
            "predicate must return a Bool, got {}",
            other.kind()
        ))),
    }
}

/// `takeWhile(seq, pred)` — a lazy `Seq` of the leading elements for which
/// `pred` holds, stopping at (and excluding) the first failure. The terminating
/// consumer for infinite sequences.
fn native_take_while(args: &[Value], env: &Env) -> Result<Value, RuntimeError> {
    let [seq, predicate] = args else {
        return Err(RuntimeError::new("takeWhile expects (seq, predicate)"));
    };
    if !matches!(seq, Value::Seq(_)) {
        return Err(RuntimeError::new(format!(
            "takeWhile expects a Seq, got {}",
            seq.kind()
        )));
    }
    Ok(take_while_seq(seq.clone(), predicate.clone(), env.clone()))
}

fn take_while_seq(seq: Value, pred: Value, env: Env) -> Value {
    Value::Seq(LazySeq::new(move || match force_seq(&seq)? {
        Step::Nil => Ok(Step::Nil),
        Step::Cons(head, tail) => {
            if keeps(&pred, &head, &env)? {
                Ok(Step::Cons(head, take_while_seq(tail, pred.clone(), env.clone())))
            } else {
                Ok(Step::Nil)
            }
        }
    }))
}

/// `fold(coll, init, f)` — reduce left-to-right, `f(acc, element)`. Forces a
/// `Seq` to the end (diverges on an infinite one — use `foldWhile` to stop).
fn native_fold(args: &[Value], env: &Env) -> Result<Value, RuntimeError> {
    let [collection, init, function] = args else {
        return Err(RuntimeError::new("fold expects (collection, init, function)"));
    };
    let mut acc = init.clone();
    if let Value::Seq(_) = collection {
        let mut current = collection.clone();
        while let Step::Cons(head, tail) = force_seq(&current)? {
            acc = apply_values(function, &[acc, head], env)?;
            current = tail;
        }
        return Ok(acc);
    }
    for item in expect_list("fold", collection)? {
        acc = apply_values(function, &[acc, item.clone()], env)?;
    }
    Ok(acc)
}

/// `join(list, sep)` — the displayed elements concatenated with `sep` between.
fn native_join(args: &[Value], _env: &Env) -> Result<Value, RuntimeError> {
    let [list, separator] = args else {
        return Err(RuntimeError::new("join expects (list, separator)"));
    };
    let Value::Str(separator) = separator else {
        return Err(RuntimeError::new(format!(
            "join separator must be a Str, got {}",
            separator.kind()
        )));
    };
    let parts = expect_list("join", list)?
        .iter()
        .map(Value::display)
        .collect::<Vec<_>>();
    Ok(Value::Str(parts.join(separator).into()))
}

#[cfg(test)]
mod tests {
    use crate::test_support::{run_modules, run_program, run_program_err, run_program_events};
    use crate::value::{TelemetryEvent, Value};

    /// Run a one-liner that uses the `Str` module, returning `main`'s value.
    fn run_str(body: &str) -> Value {
        let source = format!("use Str  main() = {body}");
        run_modules(&[("main", source.as_str())], "main")
    }

    #[test]
    fn str_upper_and_lower_change_case() {
        assert_eq!(run_str(r#"Str.upper("Hi")"#), Value::Str("HI".into()));
        assert_eq!(run_str(r#"Str.lower("Hi")"#), Value::Str("hi".into()));
    }

    #[test]
    fn str_length_counts_chars() {
        assert_eq!(run_str(r#"Str.length("hello")"#), Value::Int(5));
        assert_eq!(run_str(r#"Str.length("")"#), Value::Int(0));
    }

    #[test]
    fn str_trim_strips_surrounding_whitespace() {
        assert_eq!(run_str(r#"Str.trim("  hi  ")"#), Value::Str("hi".into()));
        assert_eq!(run_str(r#"Str.trim("hi")"#), Value::Str("hi".into()));
    }

    #[test]
    fn str_contains_is_a_substring_test() {
        assert_eq!(run_str(r#"Str.contains("hello", "ell")"#), Value::Bool(true));
        assert_eq!(run_str(r#"Str.contains("hello", "xyz")"#), Value::Bool(false));
    }

    #[test]
    fn str_contains_coexists_with_the_flat_element_contains() {
        // The payoff of namespacing: `Str.contains` (substring) and the flat
        // `contains` (element membership) are different functions, same name.
        let modules = [(
            "main",
            r#"use Str  main() = [Str.contains("abc", "b"), contains([1, 2, 3], 2)]"#,
        )];
        assert_eq!(
            run_modules(&modules, "main"),
            Value::List(vec![Value::Bool(true), Value::Bool(true)].into())
        );
    }

    #[test]
    fn str_starts_with_tests_a_prefix() {
        assert_eq!(
            run_str(r#"Str.startsWith("hello", "he")"#),
            Value::Bool(true)
        );
        assert_eq!(
            run_str(r#"Str.startsWith("hello", "lo")"#),
            Value::Bool(false)
        );
    }

    #[test]
    fn str_split_breaks_on_a_separator() {
        assert_eq!(
            run_str(r#"Str.split("a,b,c", ",")"#),
            Value::List(
                vec![
                    Value::Str("a".into()),
                    Value::Str("b".into()),
                    Value::Str("c".into()),
                ]
                .into()
            )
        );
    }

    #[test]
    fn str_replace_substitutes_every_occurrence() {
        assert_eq!(
            run_str(r#"Str.replace("a.b.c", ".", "-")"#),
            Value::Str("a-b-c".into())
        );
    }

    #[test]
    fn map_applies_a_function_to_each_element() {
        assert_eq!(
            run_program("main() = map([1, 2, 3], x -> x * 2) == [2, 4, 6]"),
            Value::Bool(true)
        );
    }

    #[test]
    fn a_finite_range_drains_to_a_list() {
        assert_eq!(
            run_program("main() = (1..4) |> toList == [1, 2, 3]"),
            Value::Bool(true)
        );
    }

    #[test]
    fn an_inclusive_range_includes_its_end() {
        assert_eq!(
            run_program("main() = (1..=3) |> toList == [1, 2, 3]"),
            Value::Bool(true)
        );
    }

    #[test]
    fn take_draws_a_prefix_of_an_infinite_range() {
        // `1..` is endless; `take` proves laziness by draining only a prefix
        // (an eager range would hang building the whole thing).
        assert_eq!(
            run_program("main() = take(3, 1..) |> toList == [1, 2, 3]"),
            Value::Bool(true)
        );
    }

    #[test]
    fn take_zero_is_empty() {
        assert_eq!(
            run_program("main() = take(0, 1..) |> toList == []"),
            Value::Bool(true)
        );
    }

    #[test]
    fn take_more_than_available_stops_at_the_end() {
        assert_eq!(
            run_program("main() = take(5, 1..3) |> toList == [1, 2]"),
            Value::Bool(true)
        );
    }

    #[test]
    fn iterate_builds_a_sequence_by_repeated_application() {
        assert_eq!(
            run_program("main() = take(4, iterate(1, x -> x * 2)) |> toList == [1, 2, 4, 8]"),
            Value::Bool(true)
        );
    }

    #[test]
    fn repeat_repeats_a_value() {
        assert_eq!(
            run_program("main() = take(3, repeat(7)) |> toList == [7, 7, 7]"),
            Value::Bool(true)
        );
    }

    #[test]
    fn map_over_a_seq_is_lazy() {
        assert_eq!(
            run_program("main() = (1..4) |> map(x -> x * 10) |> toList == [10, 20, 30]"),
            Value::Bool(true)
        );
        assert_eq!(
            run_program("main() = take(3, map(1.., x -> x * 10)) |> toList == [10, 20, 30]"),
            Value::Bool(true)
        );
    }

    #[test]
    fn filter_over_a_seq_is_lazy() {
        assert_eq!(
            run_program("main() = (1..6) |> filter(x -> x > 3) |> toList == [4, 5]"),
            Value::Bool(true)
        );
        assert_eq!(
            run_program("main() = take(2, filter(1.., x -> x % 2 == 0)) |> toList == [2, 4]"),
            Value::Bool(true)
        );
    }

    #[test]
    fn fold_over_a_finite_seq_reduces_it() {
        assert_eq!(
            run_program("main() = fold(1..5, 0, (a, b) -> a + b)"),
            Value::Int(10)
        );
    }

    #[test]
    fn fold_while_stops_when_the_step_returns_none() {
        // Sum 1.. while the running total stays ≤ 6; `None` stops, keeping the
        // accumulator from before the stopping step. Terminates over an infinite
        // sequence because the step decides to stop.
        assert_eq!(
            run_program(
                "main() = foldWhile(1.., 0, (acc, x) -> { let next = acc + x  next > 6 => None | Some(next) })"
            ),
            Value::Int(6)
        );
    }

    #[test]
    fn fold_while_that_never_stops_folds_the_whole_finite_seq() {
        assert_eq!(
            run_program("main() = foldWhile(1..4, 0, (acc, x) -> Some(acc + x))"),
            Value::Int(6)
        );
    }

    #[test]
    fn take_while_stops_at_the_first_failure() {
        // On an infinite range — terminates because `takeWhile` stops.
        assert_eq!(
            run_program("main() = (1..) |> takeWhile(x -> x < 4) |> toList == [1, 2, 3]"),
            Value::Bool(true)
        );
        assert_eq!(
            run_program("main() = (1..6) |> takeWhile(x -> x < 3) |> toList == [1, 2]"),
            Value::Bool(true)
        );
    }

    #[test]
    fn iterate_applies_its_function_lazily() {
        // `take(3, iterate(0, f))` is `[0, f(0), f(f(0))]` — `f` runs exactly
        // twice (for elements 1 and 2). A non-lazy iterate would apply `f` an
        // extra time; the emit count proves it doesn't.
        assert_eq!(
            run_program_events(
                "main() = take(3, iterate(0, x -> { emit(\"s\", x)  x + 1 })) |> toList"
            ),
            vec![
                TelemetryEvent::Emit {
                    name: "s".to_string(),
                    value: Value::Int(0)
                },
                TelemetryEvent::Emit {
                    name: "s".to_string(),
                    value: Value::Int(1)
                },
            ]
        );
    }

    #[test]
    fn filter_keeps_elements_satisfying_the_predicate() {
        assert_eq!(
            run_program("main() = filter([1, 2, 3, 4], x -> x > 2) == [3, 4]"),
            Value::Bool(true)
        );
    }

    #[test]
    fn fold_reduces_with_an_accumulator() {
        assert_eq!(
            run_program("main() = fold([1, 2, 3, 4], 0, (acc, x) -> acc + x)"),
            Value::Int(10)
        );
    }

    #[test]
    fn join_concatenates_displayed_elements_with_a_separator() {
        assert_eq!(
            run_program(r#"main() = join(["a", "b", "c"], ", ")"#),
            Value::Str("a, b, c".into())
        );
        assert_eq!(
            run_program(r#"main() = join([1, 2, 3], "-")"#),
            Value::Str("1-2-3".into())
        );
    }

    #[test]
    fn map_over_a_non_list_is_an_error() {
        assert_eq!(
            run_program_err("main() = map(5, x -> x)"),
            "map expects a List, got Int"
        );
    }

    #[test]
    fn emit_records_a_metric() {
        assert_eq!(
            run_program_events(r#"main() = emit("temp", 42)"#),
            vec![TelemetryEvent::Emit {
                name: "temp".to_string(),
                value: Value::Int(42)
            }]
        );
    }

    #[test]
    fn span_runs_its_body_and_returns_its_value() {
        assert_eq!(
            run_program(r#"main() = span("s", () -> 42)"#),
            Value::Int(42)
        );
    }

    #[test]
    fn span_brackets_its_body_with_open_and_close() {
        assert_eq!(
            run_program_events(r#"main() = span("s", () -> emit("x", 1))"#),
            vec![
                TelemetryEvent::SpanOpen {
                    name: "s".to_string()
                },
                TelemetryEvent::Emit {
                    name: "x".to_string(),
                    value: Value::Int(1)
                },
                TelemetryEvent::SpanClose {
                    name: "s".to_string()
                },
            ]
        );
    }
}
