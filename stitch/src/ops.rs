//! Operator evaluation: prefix unary and infix binary operators over
//! already-evaluated `Value`s. v0 is strict — no Int/Float coercion, so a kind
//! mismatch is a runtime error rather than a silent widening or `false`.

use std::cmp::Ordering;

use crate::ast::{BinOp, UnOp};
use crate::value::{RuntimeError, Value};

/// Apply a prefix unary operator: `-` negates an Int or Float, `not` inverts a
/// Bool. Any other operand kind is a type error.
pub(crate) fn eval_unary(op: UnOp, operand: &Value) -> Result<Value, RuntimeError> {
    match (op, operand) {
        (UnOp::Neg, Value::Int(n)) => Ok(Value::Int(-n)),
        (UnOp::Neg, Value::Float(f)) => Ok(Value::Float(-f)),
        (UnOp::Not, Value::Bool(b)) => Ok(Value::Bool(!b)),
        _ => Err(RuntimeError::new(format!(
            "operator {op:?} cannot apply to {}",
            operand.kind()
        ))),
    }
}

/// Require a value to be a `Bool`, returning a type error tagged with `context`
/// (e.g. `` "`and`" ``) otherwise.
pub(crate) fn as_bool(value: &Value, context: &str) -> Result<bool, RuntimeError> {
    match value {
        Value::Bool(b) => Ok(*b),
        other => Err(RuntimeError::new(format!(
            "{context} requires a Bool, got {}",
            other.kind()
        ))),
    }
}

/// Apply a binary operator to two already-evaluated operands. v0 is strict: no
/// Int/Float coercion, so a kind mismatch is a runtime error.
pub(crate) fn eval_binary(op: BinOp, left: &Value, right: &Value) -> Result<Value, RuntimeError> {
    match (op, left, right) {
        (BinOp::Add, Value::Int(a), Value::Int(b)) => Ok(Value::Int(a + b)),
        (BinOp::Sub, Value::Int(a), Value::Int(b)) => Ok(Value::Int(a - b)),
        (BinOp::Mul, Value::Int(a), Value::Int(b)) => Ok(Value::Int(a * b)),
        (BinOp::Div | BinOp::Rem, Value::Int(_), Value::Int(0)) => {
            Err(RuntimeError::new("division by zero"))
        }
        (BinOp::Div, Value::Int(a), Value::Int(b)) => Ok(Value::Int(a / b)),
        (BinOp::Rem, Value::Int(a), Value::Int(b)) => Ok(Value::Int(a % b)),
        // Floats follow IEEE 754: `/0.0` is ±inf, not an error.
        (BinOp::Add, Value::Float(a), Value::Float(b)) => Ok(Value::Float(a + b)),
        (BinOp::Sub, Value::Float(a), Value::Float(b)) => Ok(Value::Float(a - b)),
        (BinOp::Mul, Value::Float(a), Value::Float(b)) => Ok(Value::Float(a * b)),
        (BinOp::Div, Value::Float(a), Value::Float(b)) => Ok(Value::Float(a / b)),
        (BinOp::Rem, Value::Float(a), Value::Float(b)) => Ok(Value::Float(a % b)),
        // `+` concatenates strings (unambiguous: strict typing rules out `1 + "x"`).
        (BinOp::Add, Value::Str(a), Value::Str(b)) => Ok(Value::Str(format!("{a}{b}").into())),
        (BinOp::Eq | BinOp::Ne, _, _) => equality(op, left, right),
        (BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge, _, _) => ordering(op, left, right),
        _ => Err(type_mismatch(op, left, right)),
    }
}

/// `==` / `!=`: structural equality between same-kind operands. v0 is strict —
/// comparing across kinds (`1 == 1.0`, `1 == true`) is a type error, not `false`
/// — so we gate on the value kind, then defer to `Value`'s structural equality
/// (which compares `prod`/`sum` data by type, variant, and fields — decision D).
fn equality(op: BinOp, left: &Value, right: &Value) -> Result<Value, RuntimeError> {
    if std::mem::discriminant(left) != std::mem::discriminant(right) {
        return Err(type_mismatch(op, left, right));
    }
    let equal = left == right;
    Ok(Value::Bool(if op == BinOp::Ne { !equal } else { equal }))
}

/// A total-ish order over comparable values — two Ints, two Floats, or two Strs
/// (lexicographic). `None` for incomparable kinds (or a NaN float operand).
/// Shared by the comparison operators and `sort`.
pub(crate) fn value_order(left: &Value, right: &Value) -> Option<Ordering> {
    match (left, right) {
        (Value::Int(a), Value::Int(b)) => Some(a.cmp(b)),
        (Value::Float(a), Value::Float(b)) => a.partial_cmp(b),
        (Value::Str(a), Value::Str(b)) => Some(a.cmp(b)),
        _ => None,
    }
}

/// `<` / `<=` / `>` / `>=`: ordering on two Ints, two Floats, or two Strs. A NaN
/// operand makes every comparison `false` (IEEE 754); other kinds are a type
/// error.
fn ordering(op: BinOp, left: &Value, right: &Value) -> Result<Value, RuntimeError> {
    let comparable = matches!(
        (left, right),
        (Value::Int(_), Value::Int(_))
            | (Value::Float(_), Value::Float(_))
            | (Value::Str(_), Value::Str(_))
    );
    if !comparable {
        return Err(type_mismatch(op, left, right));
    }
    // `value_order` is `None` only for a NaN float here → every comparison false.
    let holds = value_order(left, right).is_some_and(|o| match op {
        BinOp::Lt => o == Ordering::Less,
        BinOp::Le => o != Ordering::Greater,
        BinOp::Gt => o == Ordering::Greater,
        BinOp::Ge => o != Ordering::Less,
        // `ordering` is only dispatched for the four ordering operators.
        _ => unreachable!("ordering called with non-ordering operator {op:?}"),
    });
    Ok(Value::Bool(holds))
}

/// Build the "operator can't apply to these kinds" error.
fn type_mismatch(op: BinOp, left: &Value, right: &Value) -> RuntimeError {
    RuntimeError::new(format!(
        "operator {op:?} cannot apply to {} and {}",
        left.kind(),
        right.kind()
    ))
}

#[cfg(test)]
mod tests {
    use crate::test_support::{run, run_err};
    use crate::value::Value;

    #[test]
    fn evaluates_integer_addition() {
        assert_eq!(run("1 + 2"), Value::Int(3));
    }

    #[test]
    fn evaluates_integer_subtraction_multiplication_division_remainder() {
        assert_eq!(run("10 - 3"), Value::Int(7));
        assert_eq!(run("6 * 7"), Value::Int(42));
        assert_eq!(run("7 / 2"), Value::Int(3));
        assert_eq!(run("7 % 2"), Value::Int(1));
    }

    #[test]
    fn evaluation_walks_the_parsed_precedence_tree() {
        assert_eq!(run("2 + 3 * 4"), Value::Int(14));
        assert_eq!(run("(2 + 3) * 4"), Value::Int(20));
    }

    #[test]
    fn integer_division_by_zero_is_a_runtime_error() {
        assert_eq!(run_err("1 / 0"), "division by zero");
    }

    #[test]
    fn integer_remainder_by_zero_is_a_runtime_error() {
        assert_eq!(run_err("1 % 0"), "division by zero");
    }

    #[test]
    fn evaluates_float_arithmetic() {
        assert_eq!(run("1.5 + 2.5"), Value::Float(4.0));
        assert_eq!(run("5.0 - 1.5"), Value::Float(3.5));
        assert_eq!(run("2.0 * 3.0"), Value::Float(6.0));
        assert_eq!(run("7.0 / 2.0"), Value::Float(3.5));
        assert_eq!(run("7.0 % 2.0"), Value::Float(1.0));
    }

    #[test]
    fn mixing_int_and_float_is_a_type_error() {
        assert_eq!(
            run_err("1 + 2.0"),
            "operator Add cannot apply to Int and Float"
        );
    }

    #[test]
    fn arithmetic_on_a_bool_is_a_type_error() {
        assert_eq!(
            run_err("1 + true"),
            "operator Add cannot apply to Int and Bool"
        );
    }

    #[test]
    fn evaluates_integer_comparison() {
        assert_eq!(run("1 == 1"), Value::Bool(true));
        assert_eq!(run("1 != 2"), Value::Bool(true));
        assert_eq!(run("1 < 2"), Value::Bool(true));
        assert_eq!(run("2 <= 2"), Value::Bool(true));
        assert_eq!(run("3 > 2"), Value::Bool(true));
        assert_eq!(run("2 >= 3"), Value::Bool(false));
    }

    #[test]
    fn evaluates_float_comparison() {
        assert_eq!(run("1.5 < 2.5"), Value::Bool(true));
        assert_eq!(run("2.5 == 2.5"), Value::Bool(true));
        assert_eq!(run("2.5 >= 3.5"), Value::Bool(false));
    }

    #[test]
    fn compares_strings_lexicographically() {
        assert_eq!(run(r#""a" < "b""#), Value::Bool(true));
        assert_eq!(run(r#""apple" < "apply""#), Value::Bool(true));
        assert_eq!(run(r#""b" <= "a""#), Value::Bool(false));
    }

    #[test]
    fn evaluates_bool_equality() {
        assert_eq!(run("true == true"), Value::Bool(true));
        assert_eq!(run("true != false"), Value::Bool(true));
    }

    #[test]
    fn ordering_bools_is_a_type_error() {
        assert_eq!(
            run_err("true < false"),
            "operator Lt cannot apply to Bool and Bool"
        );
    }

    #[test]
    fn comparing_across_kinds_is_a_type_error() {
        assert_eq!(
            run_err("1 == 1.0"),
            "operator Eq cannot apply to Int and Float"
        );
        assert_eq!(
            run_err("1 == true"),
            "operator Eq cannot apply to Int and Bool"
        );
    }

    #[test]
    fn evaluates_boolean_and_or() {
        assert_eq!(run("true and false"), Value::Bool(false));
        assert_eq!(run("true and true"), Value::Bool(true));
        assert_eq!(run("false or true"), Value::Bool(true));
        assert_eq!(run("false or false"), Value::Bool(false));
    }

    #[test]
    fn and_or_short_circuit_their_right_operand() {
        // `1 + true` would be a type error if evaluated; short-circuit skips it.
        assert_eq!(run("false and (1 + true)"), Value::Bool(false));
        assert_eq!(run("true or (1 + true)"), Value::Bool(true));
    }

    #[test]
    fn and_or_require_bool_operands() {
        // Only operands that are actually evaluated get type-checked. The left
        // always is; the right only when it isn't short-circuited away.
        assert_eq!(run_err("1 and true"), "`and` requires a Bool, got Int");
        assert_eq!(run_err("true and 2"), "`and` requires a Bool, got Int");
        assert_eq!(run_err("false or 2"), "`or` requires a Bool, got Int");
    }

    #[test]
    fn a_short_circuited_operand_is_not_type_checked() {
        // `2` is never evaluated, so its non-Bool type is not an error in v0.
        assert_eq!(run("true or 2"), Value::Bool(true));
        assert_eq!(run("false and 2"), Value::Bool(false));
    }

    #[test]
    fn evaluates_logical_not() {
        assert_eq!(run("not true"), Value::Bool(false));
        assert_eq!(run("not false"), Value::Bool(true));
    }

    #[test]
    fn evaluates_numeric_negation() {
        assert_eq!(run("-5"), Value::Int(-5));
        assert_eq!(run("-2.5"), Value::Float(-2.5));
    }

    #[test]
    fn unary_operators_check_their_operand_kind() {
        assert_eq!(run_err("not 1"), "operator Not cannot apply to Int");
        assert_eq!(run_err("-true"), "operator Neg cannot apply to Bool");
    }
}
