//! Tree-walk interpreter: recursively evaluate an `Expr` to a `Value`. The AST
//! *is* the program — no compilation. v0 is dynamically typed; see `value.rs`.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::rc::Rc;

use crate::ast::{Arg, BinOp, Expr, Item, Stmt, UnOp};
use crate::env::Env;
use crate::value::{ClosureData, Constructor, DataValue, RuntimeError, Value};

/// Run a program: bind every top-level function into one shared global
/// environment (so they are mutually visible — letrec), then call `main()`.
///
/// # Errors
/// Returns `Err` if there is no `main` function, or on any runtime fault.
pub fn eval_program(items: &[Item]) -> Result<Value, RuntimeError> {
    let env = Env::new();
    let mut globals = HashMap::new();
    for item in items {
        match item {
            Item::Func { name, params, body, .. } => {
                let closure = Value::Closure(Rc::new(ClosureData {
                    params: params.iter().map(|param| param.name.clone()).collect(),
                    body: body.clone(),
                    env: env.clone(),
                }));
                globals.insert(name.clone(), closure);
            }
            Item::Prod { name, fields, .. } => {
                let ctor = Value::Constructor(Rc::new(Constructor {
                    type_name: name.clone(),
                    variant: name.clone(),
                    field_names: fields.iter().map(|field| field.name.clone()).collect(),
                }));
                globals.insert(name.clone(), ctor);
            }
            _ => {}
        }
    }
    env.set_globals(globals);
    let main = env
        .lookup("main")
        .ok_or_else(|| RuntimeError::new("no `main` function"))?;
    eval_call(&main, &[], &env)
}

/// Evaluate an expression to a value in environment `env`.
///
/// # Errors
/// Returns `Err` on a runtime fault (type mismatch, division by zero, …).
pub fn eval(expr: &Expr, env: &Env) -> Result<Value, RuntimeError> {
    match expr {
        Expr::Int(n) => Ok(Value::Int(*n)),
        Expr::Float(f) => Ok(Value::Float(*f)),
        Expr::Bool(b) => Ok(Value::Bool(*b)),
        // `and`/`or` short-circuit, so they can't pre-evaluate both operands.
        Expr::Binary { op: BinOp::And, left, right } => Ok(Value::Bool(
            as_bool(&eval(left, env)?, "`and`")? && as_bool(&eval(right, env)?, "`and`")?,
        )),
        Expr::Binary { op: BinOp::Or, left, right } => Ok(Value::Bool(
            as_bool(&eval(left, env)?, "`or`")? || as_bool(&eval(right, env)?, "`or`")?,
        )),
        Expr::Binary { op, left, right } => {
            eval_binary(*op, &eval(left, env)?, &eval(right, env)?)
        }
        Expr::Unary { op, operand } => eval_unary(*op, &eval(operand, env)?),
        Expr::Var(name) => env
            .lookup(name)
            .ok_or_else(|| RuntimeError::new(format!("unbound variable `{name}`"))),
        // Only the taken branch is evaluated.
        Expr::If { cond, then, els } => {
            if as_bool(&eval(cond, env)?, "condition")? {
                eval(then, env)
            } else {
                eval(els, env)
            }
        }
        Expr::Block { stmts, result } => eval_block(stmts, result.as_deref(), env),
        Expr::Lambda { params, body } => Ok(Value::Closure(Rc::new(ClosureData {
            params: params.clone(),
            body: (**body).clone(),
            env: env.clone(),
        }))),
        Expr::Call { callee, args } => eval_call(&eval(callee, env)?, args, env),
        Expr::Field { object, name } => eval_field(&eval(object, env)?, name),
        _ => Err(RuntimeError::new("evaluation not yet implemented for this expression")),
    }
}

/// Apply `callee` to `args`. The arguments are evaluated in the caller's
/// environment, then bound to the closure's parameters on top of the
/// environment the closure captured — that captured env is what makes it a
/// closure rather than a plain function.
fn eval_call(callee: &Value, args: &[Arg], env: &Env) -> Result<Value, RuntimeError> {
    match callee {
        Value::Closure(closure) => {
            if args.len() != closure.params.len() {
                return Err(RuntimeError::new(format!(
                    "function expects {} argument(s), got {}",
                    closure.params.len(),
                    args.len()
                )));
            }
            let mut call_env = closure.env.clone();
            for (param, arg) in closure.params.iter().zip(args) {
                call_env = call_env.extend(param.clone(), eval(&arg.value, env)?);
            }
            eval(&closure.body, &call_env)
        }
        Value::Constructor(ctor) => construct(ctor, args, env),
        _ => Err(RuntimeError::new(format!("cannot call a {}", callee.kind()))),
    }
}

/// Build a `Data` value from a constructor applied to arguments. Positional
/// args fill fields in order; named args (`x: …`) fill by label in any order.
fn construct(ctor: &Constructor, args: &[Arg], env: &Env) -> Result<Value, RuntimeError> {
    let mut values: Vec<Option<Value>> = vec![None; ctor.field_names.len()];
    let mut next_positional = 0;
    for arg in args {
        // `..base` — copy every field from `base` as the starting point; later
        // args override. `base` must be a value of the same type.
        if let Expr::Spread(base) = &arg.value {
            let base = eval(base, env)?;
            let Value::Data(data) = &base else {
                return Err(RuntimeError::new(format!("can only spread a record, not {}", base.kind())));
            };
            if data.type_name != ctor.type_name {
                return Err(RuntimeError::new(format!(
                    "cannot spread a {} into {}",
                    data.type_name, ctor.type_name
                )));
            }
            for (slot, (_, value)) in values.iter_mut().zip(&data.fields) {
                *slot = Some(value.clone());
            }
            continue;
        }
        let index = if let Some(label) = &arg.label {
            ctor.field_index(label).ok_or_else(|| {
                RuntimeError::new(format!("{} has no field `{label}`", ctor.type_name))
            })?
        } else {
            let index = next_positional;
            next_positional += 1;
            index
        };
        let slot = values.get_mut(index).ok_or_else(|| {
            RuntimeError::new(format!(
                "{} expects {} field(s), got more",
                ctor.variant,
                ctor.field_names.len()
            ))
        })?;
        *slot = Some(eval(&arg.value, env)?);
    }
    let fields = ctor
        .field_names
        .iter()
        .zip(values)
        .map(|(name, value)| {
            value.map(|value| (name.clone(), value)).ok_or_else(|| {
                let field = name.clone().unwrap_or_else(|| "?".to_string());
                RuntimeError::new(format!("{} is missing field `{field}`", ctor.type_name))
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Value::Data(Rc::new(DataValue {
        type_name: ctor.type_name.clone(),
        variant: ctor.variant.clone(),
        fields,
    })))
}

/// Read field `name` from a `Data` value.
fn eval_field(object: &Value, name: &str) -> Result<Value, RuntimeError> {
    let Value::Data(data) = object else {
        return Err(RuntimeError::new(format!(
            "cannot read field `{name}` on {}",
            object.kind()
        )));
    };
    data.fields
        .iter()
        .find(|(field_name, _)| field_name.as_deref() == Some(name))
        .map(|(_, value)| value.clone())
        .ok_or_else(|| RuntimeError::new(format!("{} has no field `{name}`", data.type_name)))
}

/// Evaluate a block: thread an environment through the statements (each `let`
/// extends a fresh child scope, so bindings are visible to later statements but
/// not outside the block), then evaluate the trailing expression — or `Unit`
/// if there isn't one.
fn eval_block(stmts: &[Stmt], result: Option<&Expr>, env: &Env) -> Result<Value, RuntimeError> {
    let mut scope = env.clone();
    for stmt in stmts {
        match stmt {
            Stmt::Let { name, value, .. } => {
                let bound = eval(value, &scope)?;
                scope = scope.extend(name.clone(), bound);
            }
            Stmt::Expr(expr) => {
                eval(expr, &scope)?;
            }
            Stmt::Assign { .. } | Stmt::Use { .. } => {
                return Err(RuntimeError::new(
                    "evaluation not yet implemented for this statement",
                ));
            }
        }
    }
    match result {
        Some(expr) => eval(expr, &scope),
        None => Ok(Value::Unit),
    }
}

/// Apply a prefix unary operator: `-` negates an Int or Float, `not` inverts a
/// Bool. Any other operand kind is a type error.
fn eval_unary(op: UnOp, operand: &Value) -> Result<Value, RuntimeError> {
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
fn as_bool(value: &Value, context: &str) -> Result<bool, RuntimeError> {
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
fn eval_binary(op: BinOp, left: &Value, right: &Value) -> Result<Value, RuntimeError> {
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

/// `<` / `<=` / `>` / `>=`: ordering on two Ints or two Floats. A NaN operand
/// makes every comparison `false` (IEEE 754); other kinds are a type error.
fn ordering(op: BinOp, left: &Value, right: &Value) -> Result<Value, RuntimeError> {
    let order = match (left, right) {
        (Value::Int(a), Value::Int(b)) => Some(a.cmp(b)),
        (Value::Float(a), Value::Float(b)) => a.partial_cmp(b),
        _ => return Err(type_mismatch(op, left, right)),
    };
    let holds = order.is_some_and(|o| match op {
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
    use crate::env::Env;
    use crate::interp::{eval, eval_program};
    use crate::parser::{parse, parse_program};
    use crate::value::Value;

    /// Parse a whole program (top-level items) and run its `main`.
    fn run_program(src: &str) -> Value {
        let items = parse_program(src).expect("test program should parse");
        eval_program(&items).expect("test program should evaluate")
    }

    /// Parse and run a program, expecting a runtime error message.
    fn run_program_err(src: &str) -> String {
        let items = parse_program(src).expect("test program should parse");
        eval_program(&items)
            .expect_err("test program should fail at runtime")
            .message
    }

    /// Parse then evaluate in an empty environment, unwrapping — for tests with
    /// valid, total programs.
    fn run(src: &str) -> Value {
        eval(&parse(src).expect("test input should parse"), &Env::new())
            .expect("test input should evaluate")
    }

    #[test]
    fn evaluates_an_integer_literal() {
        assert_eq!(run("42"), Value::Int(42));
    }

    #[test]
    fn evaluates_a_float_literal() {
        assert_eq!(run("2.5"), Value::Float(2.5));
    }

    #[test]
    fn evaluates_a_bool_literal() {
        assert_eq!(run("true"), Value::Bool(true));
    }

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

    /// Parse then evaluate, expecting a runtime error message.
    fn run_err(src: &str) -> String {
        eval(&parse(src).expect("test input should parse"), &Env::new())
            .expect_err("test input should fail at runtime")
            .message
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

    #[test]
    fn a_let_binding_is_visible_in_the_block_result() {
        assert_eq!(run("{ let x = 1  x + 2 }"), Value::Int(3));
    }

    #[test]
    fn a_later_let_sees_an_earlier_binding() {
        assert_eq!(run("{ let a = 2  let b = a + 3  b }"), Value::Int(5));
    }

    #[test]
    fn an_inner_let_shadows_an_outer_one() {
        // The new binding's RHS still sees the old `x` (= 1), then shadows it.
        assert_eq!(run("{ let x = 1  let x = x + 10  x }"), Value::Int(11));
    }

    #[test]
    fn block_scope_does_not_escape() {
        assert_eq!(
            run_err("{ { let secret = 5 }  secret }"),
            "unbound variable `secret`"
        );
    }

    #[test]
    fn a_block_without_a_trailing_expression_is_unit() {
        assert_eq!(run("{ let x = 1 }"), Value::Unit);
    }

    #[test]
    fn an_unbound_variable_is_an_error() {
        assert_eq!(run_err("nope"), "unbound variable `nope`");
    }

    #[test]
    fn evaluates_the_conditional_expression() {
        assert_eq!(run("1 < 2 => 10 | 20"), Value::Int(10));
        assert_eq!(run("1 > 2 => 10 | 20"), Value::Int(20));
    }

    #[test]
    fn the_conditional_evaluates_only_the_taken_branch() {
        // The untaken branch (`1 + true`) would error if evaluated.
        assert_eq!(run("true => 1 | (1 + true)"), Value::Int(1));
        assert_eq!(run("false => (1 + true) | 2"), Value::Int(2));
    }

    #[test]
    fn a_non_bool_condition_is_an_error() {
        assert_eq!(run_err("1 => 10 | 20"), "condition requires a Bool, got Int");
    }

    #[test]
    fn applies_a_lambda_to_an_argument() {
        assert_eq!(run("(x -> x + 1)(5)"), Value::Int(6));
    }

    #[test]
    fn applies_a_multi_parameter_lambda() {
        assert_eq!(run("((a, b) -> a + b)(3, 4)"), Value::Int(7));
    }

    #[test]
    fn a_closure_captures_its_defining_environment() {
        assert_eq!(run("{ let n = 10  let add = x -> x + n  add(5) }"), Value::Int(15));
    }

    #[test]
    fn closures_capture_lexically_not_dynamically() {
        // `f` closes over the outer `n` (10); the inner block's `n` (99) is a
        // different binding and must not affect the captured value.
        assert_eq!(
            run("{ let n = 10  let f = () -> n  { let n = 99  f() } }"),
            Value::Int(10)
        );
    }

    #[test]
    fn higher_order_function_returns_a_closure() {
        assert_eq!(
            run("{ let twice = f -> (x -> f(f(x)))  let inc = n -> n + 1  twice(inc)(10) }"),
            Value::Int(12)
        );
    }

    #[test]
    fn a_placeholder_argument_becomes_a_callable_closure() {
        // `($ + 1)` desugars (at parse time) to `$a -> $a + 1`, then `apply`
        // calls it with 10.
        assert_eq!(run("{ let apply = g -> g(10)  apply($ + 1) }"), Value::Int(11));
    }

    #[test]
    fn calling_a_non_function_is_an_error() {
        assert_eq!(run_err("5(3)"), "cannot call a Int");
    }

    #[test]
    fn an_arity_mismatch_is_an_error() {
        assert_eq!(
            run_err("(x -> x)(1, 2)"),
            "function expects 1 argument(s), got 2"
        );
    }

    #[test]
    fn runs_a_recursive_top_level_function() {
        assert_eq!(
            run_program("fact(n) = n == 0 => 1 | n * fact(n - 1)  main() = fact(5)"),
            Value::Int(120)
        );
    }

    #[test]
    fn a_top_level_function_calls_another() {
        assert_eq!(
            run_program("double(x) = x * 2  main() = double(21)"),
            Value::Int(42)
        );
    }

    #[test]
    fn supports_mutual_recursion() {
        assert_eq!(
            run_program(
                "isEven(n) = n == 0 => true | isOdd(n - 1)  \
                 isOdd(n) = n == 0 => false | isEven(n - 1)  \
                 main() = isEven(4)"
            ),
            Value::Bool(true)
        );
    }

    #[test]
    fn a_program_without_main_is_an_error() {
        let items = parse_program("foo() = 1").expect("should parse");
        assert_eq!(
            eval_program(&items).expect_err("should fail").message,
            "no `main` function"
        );
    }

    #[test]
    fn constructs_a_prod_and_reads_its_fields() {
        assert_eq!(
            run_program("prod Point(x: Int, y: Int)  main() = Point(1, 2).x"),
            Value::Int(1)
        );
        assert_eq!(
            run_program("prod Point(x: Int, y: Int)  main() = Point(1, 2).y"),
            Value::Int(2)
        );
    }

    #[test]
    fn reading_a_missing_field_is_an_error() {
        assert_eq!(
            run_program_err("prod Point(x: Int, y: Int)  main() = Point(1, 2).z"),
            "Point has no field `z`"
        );
    }

    #[test]
    fn constructs_a_prod_with_named_arguments_in_any_order() {
        assert_eq!(
            run_program("prod Point(x: Int, y: Int)  main() = Point(y: 2, x: 1).x"),
            Value::Int(1)
        );
    }

    #[test]
    fn an_unknown_field_label_is_an_error() {
        assert_eq!(
            run_program_err("prod Point(x: Int, y: Int)  main() = Point(x: 1, z: 9)"),
            "Point has no field `z`"
        );
    }

    #[test]
    fn a_missing_field_in_construction_is_an_error() {
        assert_eq!(
            run_program_err("prod Point(x: Int, y: Int)  main() = Point(x: 1)"),
            "Point is missing field `y`"
        );
    }

    #[test]
    fn functional_update_copies_then_overrides() {
        assert_eq!(
            run_program("prod Point(x: Int, y: Int)  main() = { let p = Point(1, 2)  Point(..p, x: 10).x }"),
            Value::Int(10)
        );
        assert_eq!(
            run_program("prod Point(x: Int, y: Int)  main() = { let p = Point(1, 2)  Point(..p, x: 10).y }"),
            Value::Int(2)
        );
    }

    #[test]
    fn prods_have_structural_equality() {
        assert_eq!(
            run_program("prod Point(x: Int, y: Int)  main() = Point(1, 2) == Point(1, 2)"),
            Value::Bool(true)
        );
        assert_eq!(
            run_program("prod Point(x: Int, y: Int)  main() = Point(1, 2) == Point(1, 9)"),
            Value::Bool(false)
        );
    }
}
