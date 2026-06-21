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
];

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
fn expect_list<'a>(name: &str, value: &'a Value) -> Result<&'a [Value], RuntimeError> {
    match value {
        Value::List(items) => Ok(items),
        other => Err(RuntimeError::new(format!(
            "{name} expects a List, got {}",
            other.kind()
        ))),
    }
}

/// `map(list, f)` — a new list with `f` applied to each element.
fn native_map(args: &[Value], env: &Env) -> Result<Value, RuntimeError> {
    let [list, function] = args else {
        return Err(RuntimeError::new("map expects (list, function)"));
    };
    let mapped = expect_list("map", list)?
        .iter()
        .map(|item| apply_values(function, std::slice::from_ref(item), env))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Value::List(mapped.into()))
}

/// `filter(list, pred)` — the elements for which `pred` returns `true`.
fn native_filter(args: &[Value], env: &Env) -> Result<Value, RuntimeError> {
    let [list, predicate] = args else {
        return Err(RuntimeError::new("filter expects (list, predicate)"));
    };
    let mut kept = Vec::new();
    for item in expect_list("filter", list)? {
        match apply_values(predicate, std::slice::from_ref(item), env)? {
            Value::Bool(true) => kept.push(item.clone()),
            Value::Bool(false) => {}
            other => {
                return Err(RuntimeError::new(format!(
                    "filter predicate must return a Bool, got {}",
                    other.kind()
                )));
            }
        }
    }
    Ok(Value::List(kept.into()))
}

/// `fold(list, init, f)` — reduce left-to-right, `f(acc, element)`.
fn native_fold(args: &[Value], env: &Env) -> Result<Value, RuntimeError> {
    let [list, init, function] = args else {
        return Err(RuntimeError::new("fold expects (list, init, function)"));
    };
    let mut acc = init.clone();
    for item in expect_list("fold", list)? {
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
    use crate::test_support::{run_program, run_program_err, run_program_events};
    use crate::value::{TelemetryEvent, Value};

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
