//! Tree-walk interpreter: recursively evaluate an `Expr` to a `Value`. The AST
//! *is* the program — no compilation. v0 is dynamically typed; see `value.rs`.

use std::rc::Rc;

use crate::ast::{Arg, BinOp, Expr, Item, MethodModifier, Stmt, StrSegment};
use crate::env::{AssignError, Env};
use crate::natives::NATIVES;
use crate::ops::{as_bool, eval_binary, eval_unary};
use crate::parser::parse_program;
use crate::pattern::eval_match;
use crate::registry::{Registration, bake_contract_defaults, register_builtin_types, register_items};
use crate::value::{ClosureData, Constructor, DataValue, RuntimeError, TelemetryEvent, Value};

/// Run a program: bind every top-level function into one shared global
/// environment (so they are mutually visible — letrec), then call `main()`.
///
/// # Errors
/// Returns `Err` if there is no `main` function, or on any runtime fault.
pub fn eval_program(items: &[Item]) -> Result<Value, RuntimeError> {
    eval_program_with_telemetry(items).0
}

/// Like [`eval_program`], but also returns the telemetry (`emit`/`span`)
/// recorded during the run — the observable output of the program.
pub fn eval_program_with_telemetry(
    items: &[Item],
) -> (Result<Value, RuntimeError>, Vec<TelemetryEvent>) {
    let env = Env::new();
    let mut reg = Registration::default();
    for native in NATIVES {
        reg.globals.insert(native.name.to_string(), Value::Native(*native));
    }
    register_builtin_types(&mut reg.globals);
    // The Stitch-source prelude loads first; user items can shadow it.
    let prelude = parse_program(PRELUDE).expect("the prelude must parse");
    register_items(&prelude, &env, &mut reg);
    register_items(items, &env, &mut reg);
    // After every `on`/`contract` is collected, fold contract default methods
    // into the types that conform — a concrete impl already present wins.
    bake_contract_defaults(&mut reg);
    env.set_globals(reg.globals);
    env.set_methods(reg.methods);
    env.set_field_mut(reg.field_mut);
    let result = match env.lookup("main") {
        Some(main) => eval_call(&main, &[], &env),
        None => Err(RuntimeError::new("no `main` function")),
    };
    (result, env.telemetry())
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
        Expr::Str(segments) => eval_string(segments, env),
        // `and`/`or` short-circuit, so they can't pre-evaluate both operands.
        Expr::Binary {
            op: BinOp::And,
            left,
            right,
        } => Ok(Value::Bool(
            as_bool(&eval(left, env)?, "`and`")? && as_bool(&eval(right, env)?, "`and`")?,
        )),
        Expr::Binary {
            op: BinOp::Or,
            left,
            right,
        } => Ok(Value::Bool(
            as_bool(&eval(left, env)?, "`or`")? || as_bool(&eval(right, env)?, "`or`")?,
        )),
        // `lhs |> f(a)` ≡ `f(lhs, a)`; `lhs |> f` ≡ `f(lhs)`.
        Expr::Binary {
            op: BinOp::Pipe,
            left,
            right,
        } => eval_pipe(left, right, env),
        Expr::Binary { op, left, right } => eval_binary(*op, &eval(left, env)?, &eval(right, env)?),
        Expr::Unary { op, operand } => eval_unary(*op, &eval(operand, env)?),
        Expr::Var(name) => env
            .lookup(name)
            .ok_or_else(|| RuntimeError::new(format!("unbound variable `{name}`"))),
        Expr::SelfRef => env
            .lookup("@")
            .ok_or_else(|| RuntimeError::new("`@` is only valid inside a method body")),
        // Only the taken branch is evaluated.
        Expr::If { cond, then, els } => {
            if as_bool(&eval(cond, env)?, "condition")? {
                eval(then, env)
            } else {
                eval(els, env)
            }
        }
        Expr::Block { stmts, result } => eval_block(stmts, result.as_deref(), env),
        Expr::Match { subject, arms } => eval_match(&eval(subject, env)?, arms, env),
        // `()` is unit; `(a, b, …)` is a tuple.
        Expr::Tuple(elements) if elements.is_empty() => Ok(Value::Unit),
        Expr::Tuple(elements) => {
            let values = elements
                .iter()
                .map(|element| eval(element, env))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Value::Tuple(values.into()))
        }
        Expr::List(elements) => {
            let values = elements
                .iter()
                .map(|element| eval(element, env))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Value::List(values.into()))
        }
        Expr::Map(entries) => {
            let mut map: Vec<(Value, Value)> = Vec::new();
            for (key_expr, value_expr) in entries {
                let key = eval(key_expr, env)?;
                let value = eval(value_expr, env)?;
                // Last duplicate key wins; keep the first occurrence's position.
                if let Some(slot) = map.iter_mut().find(|(existing, _)| *existing == key) {
                    slot.1 = value;
                } else {
                    map.push((key, value));
                }
            }
            Ok(Value::Map(Rc::new(map)))
        }
        Expr::Lambda { params, body } => Ok(Value::Closure(Rc::new(ClosureData {
            params: params.clone(),
            body: (**body).clone(),
            env: env.clone(),
        }))),
        // `receiver.method(args)` parses as a call whose callee is a field
        // access (there is no dedicated method-call node). Intercept that shape
        // *before* evaluating the callee — `receiver.method` isn't a value on
        // its own; the receiver and the name must be resolved together against
        // the method registry. Any other call falls through to `eval_call`.
        Expr::Call { callee, args } => match callee.as_ref() {
            Expr::Field { object, name } => eval_method_call(object, name, args, env),
            _ => eval_call(&eval(callee, env)?, args, env),
        },
        Expr::Field { object, name } => eval_field(&eval(object, env)?, name),
        Expr::Try(operand) => eval_try(eval(operand, env)?, env),
        Expr::SafeField { object, name } => eval_safe_field(&eval(object, env)?, name, env),
        Expr::Index { object, index } => eval_index(&eval(object, env)?, &eval(index, env)?),
        _ => Err(RuntimeError::new(
            "evaluation not yet implemented for this expression",
        )),
    }
}

/// Apply `callee` to `args`. The arguments are evaluated in the caller's
/// environment, then bound to the closure's parameters on top of the
/// environment the closure captured — that captured env is what makes it a
/// closure rather than a plain function.
fn eval_call(callee: &Value, args: &[Arg], env: &Env) -> Result<Value, RuntimeError> {
    // Constructors are the only callees that take named args / `..` spread.
    if let Value::Constructor(ctor) = callee {
        return construct(ctor, args, env);
    }
    let mut values = Vec::with_capacity(args.len());
    for arg in args {
        if arg.label.is_some() {
            return Err(RuntimeError::new(
                "named arguments are only allowed when constructing a prod/variant",
            ));
        }
        if matches!(arg.value, Expr::Spread(_)) {
            return Err(RuntimeError::new(
                "spread (`..`) is only allowed when constructing a prod/variant",
            ));
        }
        values.push(eval(&arg.value, env)?);
    }
    apply_values(callee, &values, env)
}

/// Dispatch a method call `object.name(args)`. There is no method-call AST node,
/// so this is reached from the `Call { callee: Field }` interception in `eval`.
///
/// The lookup is type-directed (not lexical): evaluate the receiver, find the
/// method registered for its type, then run the method's body in a fresh global
/// scope — globals and other methods are reachable, but the *caller's* locals
/// are not, the same hygiene a closure gets from its captured env. The receiver
/// is bound as `@` and the arguments to the method's parameters.
fn eval_method_call(
    object: &Expr,
    name: &str,
    args: &[Arg],
    env: &Env,
) -> Result<Value, RuntimeError> {
    // Resolve what we're dispatching on. The object is usually evaluated to a
    // value — a `Data` instance binds `@`; a `Constructor` is the type itself
    // (for a `free` method, no `@`). Special case (Java-style type path): a bare
    // name with no value that names a type with methods (a `sum`'s type name has
    // no value, only its variants do) is resolved *as a type*, not evaluated.
    let (type_name, self_value): (String, Option<Value>) = match object {
        Expr::Var(type_path) if env.lookup(type_path).is_none() && env.has_methods(type_path) => {
            (type_path.clone(), None)
        }
        _ => {
            let receiver = eval(object, env)?;
            match receiver {
                Value::Data(ref data) => (data.type_name.clone(), Some(receiver.clone())),
                Value::Constructor(ref ctor) => (ctor.type_name.clone(), None),
                other => {
                    return Err(RuntimeError::new(format!(
                        "cannot call method `{name}` on {}",
                        other.kind()
                    )));
                }
            }
        }
    };
    let Some(method) = env.lookup_method(&type_name, name) else {
        // No method by that name. If the receiver is a record with a field of
        // that name, this is a *field call* — read the field and apply it (a
        // record may hold a closure). Methods take precedence; this fallback
        // only runs when no method shadows the field. (Only an instance receiver
        // has fields; a type-path/`Constructor` receiver falls through.)
        if let Some(receiver) = &self_value
            && let Ok(field_value) = eval_field(receiver, name)
        {
            let mut values = Vec::with_capacity(args.len());
            for arg in args {
                values.push(eval(&arg.value, env)?);
            }
            return apply_values(&field_value, &values, env);
        }
        return Err(RuntimeError::new(format!(
            "{type_name} has no method `{name}`"
        )));
    };

    // The modifier must match how the method was reached: `free` on the type, an
    // instance method on a value.
    match method.modifier {
        MethodModifier::Free if self_value.is_some() => {
            return Err(RuntimeError::new(format!(
                "free method `{name}` is called on the type `{type_name}`, not an instance"
            )));
        }
        MethodModifier::Instance | MethodModifier::Mut if self_value.is_none() => {
            return Err(RuntimeError::new(format!(
                "method `{name}` needs an instance receiver — call it on a value"
            )));
        }
        _ => {}
    }

    if args.len() != method.params.len() {
        return Err(RuntimeError::new(format!(
            "method `{name}` expects {} argument(s), got {}",
            method.params.len(),
            args.len()
        )));
    }

    // A `mut` method writes its receiver back, so the receiver must be an
    // assignable place — reject a temporary up front (before any side effects),
    // rather than letting the write-back fail with a confusing message.
    let is_mut = matches!(method.modifier, MethodModifier::Mut);
    if is_mut && !is_assignable_place(object) {
        return Err(RuntimeError::new(format!(
            "cannot call mut method `{name}` on a temporary — it has no place to write back to"
        )));
    }

    // Arguments evaluate in the caller's scope; the body runs in global scope.
    // An instance method binds the receiver as `@`; a `free` method doesn't. A
    // `mut` method binds `@` mutably so its body can reassign `@`/`@field`, and
    // the result is written back to the caller afterwards (value semantics, so
    // mutation isn't shared until we reassign the caller's place).
    let mut method_env = env.globals_only();
    if let Some(receiver) = self_value {
        method_env = if is_mut {
            method_env.extend_mut("@".to_string(), receiver)
        } else {
            method_env.extend("@".to_string(), receiver)
        };
    }
    for (param, arg) in method.params.iter().zip(args) {
        method_env = method_env.extend(param.name.clone(), eval(&arg.value, env)?);
    }

    let body = method
        .body
        .as_ref()
        .expect("an `on`-block method always has a body (parser-enforced)");
    // A method is a call boundary, so a `?` early-return stops here (like a
    // closure call) rather than escaping the method.
    let result = match eval(body, &method_env) {
        Err(RuntimeError::Return(value)) => value,
        other => other?,
    };

    if is_mut {
        let mutated = method_env
            .lookup("@")
            .expect("a mut method binds `@` for its (instance) receiver");
        assign_place(object, mutated, env)?;
    }
    Ok(result)
}

/// Evaluate a pipe `left |> right`. If `right` is a call `f(a, …)`, insert
/// `left` as the first argument: `f(left, a, …)`. Otherwise `right` is a bare
/// function reference and is applied to `left` alone.
fn eval_pipe(left: &Expr, right: &Expr, env: &Env) -> Result<Value, RuntimeError> {
    let piped = eval(left, env)?;
    if let Expr::Call { callee, args } = right {
        let function = eval(callee, env)?;
        let mut values = Vec::with_capacity(args.len() + 1);
        values.push(piped);
        for arg in args {
            if arg.label.is_some() || matches!(arg.value, Expr::Spread(_)) {
                return Err(RuntimeError::new(
                    "a piped call takes positional arguments only",
                ));
            }
            values.push(eval(&arg.value, env)?);
        }
        apply_values(&function, &values, env)
    } else {
        apply_values(&eval(right, env)?, &[piped], env)
    }
}

/// Apply a callable to already-evaluated positional arguments. Shared by
/// `eval_call`, pipes, `use`, and the native combinators. `env` is threaded so
/// native functions can reach the telemetry sink (closures use their own
/// captured environment, not this one).
pub(crate) fn apply_values(callee: &Value, args: &[Value], env: &Env) -> Result<Value, RuntimeError> {
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
                call_env = call_env.extend(param.clone(), arg.clone());
            }
            // This is a function boundary, so `?`'s early-return stops here and
            // becomes the call's value.
            match eval(&closure.body, &call_env) {
                Err(RuntimeError::Return(value)) => Ok(value),
                other => other,
            }
        }
        Value::Native(native) => {
            if args.len() != native.arity {
                return Err(RuntimeError::new(format!(
                    "{} expects {} argument(s), got {}",
                    native.name,
                    native.arity,
                    args.len()
                )));
            }
            (native.func)(args, env)
        }
        Value::Constructor(ctor) => make_data(ctor, args),
        _ => Err(RuntimeError::new(format!(
            "cannot call a {}",
            callee.kind()
        ))),
    }
}

/// The Stitch-source prelude, compiled into the binary and loaded before user code.
const PRELUDE: &str = include_str!("prelude.st");

/// Build a `Data` from a constructor and its field values, in declaration order.
fn make_data(ctor: &Constructor, values: &[Value]) -> Result<Value, RuntimeError> {
    if values.len() != ctor.field_names.len() {
        return Err(RuntimeError::new(format!(
            "{} expects {} field(s), got {}",
            ctor.variant,
            ctor.field_names.len(),
            values.len()
        )));
    }
    let fields = ctor
        .field_names
        .iter()
        .cloned()
        .zip(values.iter().cloned())
        .collect();
    Ok(Value::Data(Rc::new(DataValue {
        type_name: ctor.type_name.clone(),
        variant: ctor.variant.clone(),
        fields,
    })))
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
                return Err(RuntimeError::new(format!(
                    "can only spread a record, not {}",
                    base.kind()
                )));
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
    let ordered = ctor
        .field_names
        .iter()
        .zip(values)
        .map(|(name, value)| {
            value.ok_or_else(|| {
                let field = name.clone().unwrap_or_else(|| "?".to_string());
                RuntimeError::new(format!("{} is missing field `{field}`", ctor.type_name))
            })
        })
        .collect::<Result<Vec<Value>, _>>()?;
    make_data(ctor, &ordered)
}

/// Evaluate a string literal: concatenate literal segments and the displayed
/// value of each `{expr}` interpolation.
fn eval_string(segments: &[StrSegment], env: &Env) -> Result<Value, RuntimeError> {
    let mut text = String::new();
    for segment in segments {
        match segment {
            StrSegment::Lit(literal) => text.push_str(literal),
            StrSegment::Interp(expr) => text.push_str(&eval(expr, env)?.display()),
        }
    }
    Ok(Value::Str(text.into()))
}

/// Build a `Maybe`: `Some(value)`.
fn some(value: Value) -> Value {
    Value::Data(Rc::new(DataValue {
        type_name: "Maybe".to_string(),
        variant: "Some".to_string(),
        fields: vec![(None, value)],
    }))
}

/// Build a `Maybe`: `None`.
fn none() -> Value {
    Value::Data(Rc::new(DataValue {
        type_name: "Maybe".to_string(),
        variant: "None".to_string(),
        fields: Vec::new(),
    }))
}

/// Index a collection: `map[key]` looks up by key, `list[i]` by position. Both
/// return a `Maybe` — `None` for a missing key or out-of-range index (no null).
fn eval_index(object: &Value, index: &Value) -> Result<Value, RuntimeError> {
    match object {
        Value::Map(entries) => Ok(entries
            .iter()
            .find(|(key, _)| key == index)
            .map_or_else(none, |(_, value)| some(value.clone()))),
        Value::List(items) => {
            let Value::Int(position) = index else {
                return Err(RuntimeError::new(format!(
                    "list index must be an Int, got {}",
                    index.kind()
                )));
            };
            let element = usize::try_from(*position).ok().and_then(|i| items.get(i));
            Ok(element.map_or_else(none, |value| some(value.clone())))
        }
        other => Err(RuntimeError::new(format!(
            "cannot index a {}",
            other.kind()
        ))),
    }
}

/// Dispatch an instance method on an already-evaluated value, binding it as `@`
/// and `args` to the parameters, running the body in global scope (like
/// `eval_method_call`, minus the call-site machinery). Returns `Ok(None)` when
/// the value's type has no such method, so a caller can report a contract-specific
/// error. Used by the operators that desugar to method calls (`?` → `Try`,
/// `?.` → `Functor`).
fn call_instance_method(
    receiver: &Value,
    name: &str,
    args: &[Value],
    env: &Env,
) -> Result<Option<Value>, RuntimeError> {
    let Value::Data(data) = receiver else {
        return Ok(None);
    };
    let Some(method) = env.lookup_method(&data.type_name, name) else {
        return Ok(None);
    };
    let Some(body) = method.body.as_ref() else {
        return Ok(None);
    };
    let mut method_env = env.globals_only().extend("@".to_string(), receiver.clone());
    for (param, arg) in method.params.iter().zip(args) {
        method_env = method_env.extend(param.name.clone(), arg.clone());
    }
    // A method is a call boundary, so a `?` inside it stops here.
    let result = match eval(body, &method_env) {
        Err(RuntimeError::Return(value)) => value,
        other => other?,
    };
    Ok(Some(result))
}

/// The `?` try operator, desugared over the `Try` contract: ask the value if it
/// is the failure case (`isFailure`); if so, short-circuit the enclosing function
/// by returning it unchanged (via `RuntimeError::Return`, caught at the closure
/// boundary in `apply_values`); otherwise `unwrap` the success payload. Std
/// implements `Try` for Maybe (failure = `None`) and Result (failure = `Err`) in
/// the prelude; any user type with a success/failure split can opt in.
fn eval_try(value: Value, env: &Env) -> Result<Value, RuntimeError> {
    let Some(is_failure) = call_instance_method(&value, "isFailure", &[], env)? else {
        return Err(RuntimeError::new(format!(
            "`?` expects a value implementing `Try` (e.g. Maybe/Result), got {}",
            value.kind()
        )));
    };
    match is_failure {
        Value::Bool(true) => Err(RuntimeError::early_return(value)),
        Value::Bool(false) => call_instance_method(&value, "unwrap", &[], env)?
            .ok_or_else(|| RuntimeError::new("`Try` value implements isFailure but not unwrap")),
        other => Err(RuntimeError::new(format!(
            "`Try.isFailure` must return a Bool, got {}",
            other.kind()
        ))),
    }
}

/// The `?.` safe-navigation operator, desugared over the `Functor` contract:
/// `x?.field` ≡ `x.map(v -> v.field)`. Unlike `?` (which unwraps), `?.` *re-wraps*
/// — `map` keeps the container shape, so `Some(v)?.y` → `Some(v.y)` and
/// `None?.y` → `None`. Std implements `Functor` for `Maybe`/`Result` in the
/// prelude; a user container opts in by implementing `map`.
fn eval_safe_field(object: &Value, name: &str, env: &Env) -> Result<Value, RuntimeError> {
    // The accessor `v -> v.field`, handed to `map` as the function to apply
    // inside the container.
    let accessor = Value::Closure(Rc::new(ClosureData {
        params: vec!["v".to_string()],
        body: Expr::Field {
            object: Box::new(Expr::Var("v".to_string())),
            name: name.to_string(),
        },
        env: env.clone(),
    }));
    call_instance_method(object, "map", std::slice::from_ref(&accessor), env)?.ok_or_else(|| {
        RuntimeError::new(format!(
            "`?.` expects a value implementing `Functor` (e.g. Maybe/Result), got {}",
            object.kind()
        ))
    })
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

/// Assign `value` to a place expression (an lvalue): a variable, the receiver
/// `@`, or a field path rooted at one of those. Records are immutable, so a
/// field assignment doesn't mutate in place — it rebuilds the containing record
/// with the field replaced and reassigns the *root binding* (value semantics).
/// A nested path (`a.b.x = v`) recurses: rebuild `b`, then assign `a.b`.
fn assign_place(place: &Expr, value: Value, scope: &Env) -> Result<(), RuntimeError> {
    match place {
        Expr::Var(name) => assign_binding(name, value, scope),
        Expr::SelfRef => assign_binding("@", value, scope),
        Expr::Field { object, name } => {
            let current = eval(object, scope)?;
            let Value::Data(data) = &current else {
                return Err(RuntimeError::new(format!(
                    "cannot assign field `{name}` on {}",
                    current.kind()
                )));
            };
            // The field-mutability table doubles as the existence check: an
            // unknown field has no entry; a known-but-immutable one is `false`.
            match scope.field_mutability(&data.variant, name) {
                None => {
                    return Err(RuntimeError::new(format!(
                        "{} has no field `{name}`",
                        data.type_name
                    )));
                }
                Some(false) => {
                    return Err(RuntimeError::new(format!(
                        "cannot assign to immutable field `{name}` of `{}` (declare it `mut`)",
                        data.type_name
                    )));
                }
                Some(true) => {}
            }
            assign_place(object, rebuild_with_field(data, name, value)?, scope)
        }
        _ => Err(RuntimeError::new("invalid assignment target")),
    }
}

/// Whether `expr` is an assignable place: a variable, `@`, or a field path
/// rooted at one. (A temporary like `Counter(0)` or a literal is not.)
fn is_assignable_place(expr: &Expr) -> bool {
    match expr {
        Expr::Var(_) | Expr::SelfRef => true,
        Expr::Field { object, .. } => is_assignable_place(object),
        _ => false,
    }
}

/// Reassign a named binding (or the receiver `@`), mapping the env's assignment
/// errors to messages.
fn assign_binding(name: &str, value: Value, scope: &Env) -> Result<(), RuntimeError> {
    match scope.assign(name, value) {
        Ok(()) => Ok(()),
        Err(AssignError::Unbound) => Err(RuntimeError::new(format!(
            "cannot assign to undefined variable `{name}`"
        ))),
        Err(AssignError::Immutable) => Err(RuntimeError::new(format!(
            "cannot assign to immutable `{name}` (declare it with `let mut`)"
        ))),
    }
}

/// A copy of `data` with field `name` replaced by `value`. Errors if the record
/// has no such field.
fn rebuild_with_field(
    data: &DataValue,
    name: &str,
    value: Value,
) -> Result<Value, RuntimeError> {
    let mut fields = data.fields.clone();
    let slot = fields
        .iter_mut()
        .find(|(field_name, _)| field_name.as_deref() == Some(name));
    let Some((_, old)) = slot else {
        return Err(RuntimeError::new(format!(
            "{} has no field `{name}`",
            data.type_name
        )));
    };
    *old = value;
    Ok(Value::Data(Rc::new(DataValue {
        type_name: data.type_name.clone(),
        variant: data.variant.clone(),
        fields,
    })))
}

/// Evaluate a block: thread an environment through the statements (each `let`
/// extends a fresh child scope, so bindings are visible to later statements but
/// not outside the block), then evaluate the trailing expression — or `Unit`
/// if there isn't one.
fn eval_block(stmts: &[Stmt], result: Option<&Expr>, env: &Env) -> Result<Value, RuntimeError> {
    let mut scope = env.clone();
    for (index, stmt) in stmts.iter().enumerate() {
        match stmt {
            Stmt::Let {
                name,
                mutable,
                value,
            } => {
                let bound = eval(value, &scope)?;
                scope = if *mutable {
                    scope.extend_mut(name.clone(), bound)
                } else {
                    scope.extend(name.clone(), bound)
                };
            }
            Stmt::Expr(expr) => {
                eval(expr, &scope)?;
            }
            // `use x <- f(a)` turns the rest of the block into a callback and
            // appends it to the call: `f(a, x -> { rest })` (Gleam-style).
            Stmt::Use { binding, call } => {
                let rest = Expr::Block {
                    stmts: stmts[index + 1..].to_vec(),
                    result: result.map(|expr| Box::new(expr.clone())),
                };
                let callback = Value::Closure(Rc::new(ClosureData {
                    params: binding.iter().cloned().collect(),
                    body: rest,
                    env: scope.clone(),
                }));
                return apply_use(call, callback, &scope);
            }
            Stmt::Assign { target, value } => {
                let new_value = eval(value, &scope)?;
                assign_place(target, new_value, &scope)?;
            }
        }
    }
    match result {
        Some(expr) => eval(expr, &scope),
        None => Ok(Value::Unit),
    }
}

/// Apply a `use`'s call with the rest-of-block `callback` appended as the final
/// argument: `f(a)` becomes `f(a, callback)`, `f` becomes `f(callback)`.
fn apply_use(call: &Expr, callback: Value, env: &Env) -> Result<Value, RuntimeError> {
    if let Expr::Call { callee, args } = call {
        let function = eval(callee, env)?;
        let mut values = Vec::with_capacity(args.len() + 1);
        for arg in args {
            if arg.label.is_some() || matches!(arg.value, Expr::Spread(_)) {
                return Err(RuntimeError::new(
                    "a `use` call takes positional arguments only",
                ));
            }
            values.push(eval(&arg.value, env)?);
        }
        values.push(callback);
        apply_values(&function, &values, env)
    } else {
        apply_values(&eval(call, env)?, &[callback], env)
    }
}

#[cfg(test)]
mod tests {
    use crate::interp::eval_program;
    use crate::parser::parse_program;
    use crate::test_support::{run, run_err, run_program, run_program_err, run_program_events};
    use crate::value::{TelemetryEvent, Value};

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
    fn a_mut_binding_can_be_reassigned() {
        assert_eq!(
            run("{ let mut n = 0  n = n + 1  n = n + 1  n }"),
            Value::Int(2)
        );
    }

    #[test]
    fn reassigning_an_immutable_binding_is_an_error() {
        assert_eq!(
            run_err("{ let x = 1  x = 2  x }"),
            "cannot assign to immutable `x` (declare it with `let mut`)"
        );
    }

    #[test]
    fn assigning_a_non_mut_field_is_an_error() {
        // `x` isn't declared `mut`, so it can't be assigned even on a `mut`
        // binding — per-field mutability, like the `mut balance` in the design.
        assert_eq!(
            run_program_err(
                "prod Point(x: Int, mut y: Int)  \
                 main() = { let mut p = Point(1, 2)  p.x = 10  p.x }"
            ),
            "cannot assign to immutable field `x` of `Point` (declare it `mut`)"
        );
    }

    #[test]
    fn a_field_can_be_assigned_on_a_mut_binding() {
        assert_eq!(
            run_program(
                "prod Point(mut x: Int, mut y: Int)  \
                 main() = { let mut p = Point(1, 2)  p.x = 10  p.x }"
            ),
            Value::Int(10)
        );
    }

    #[test]
    fn assigning_a_field_leaves_other_fields_unchanged() {
        assert_eq!(
            run_program(
                "prod Point(mut x: Int, mut y: Int)  \
                 main() = { let mut p = Point(1, 2)  p.x = 10  p.y }"
            ),
            Value::Int(2)
        );
    }

    #[test]
    fn assigning_a_field_on_an_immutable_binding_is_an_error() {
        assert_eq!(
            run_program_err(
                "prod Point(mut x: Int, y: Int)  main() = { let p = Point(1, 2)  p.x = 10  p.x }"
            ),
            "cannot assign to immutable `p` (declare it with `let mut`)"
        );
    }

    #[test]
    fn assigning_an_unknown_field_is_an_error() {
        assert_eq!(
            run_program_err(
                "prod Point(x: Int, y: Int)  main() = { let mut p = Point(1, 2)  p.z = 10  p.x }"
            ),
            "Point has no field `z`"
        );
    }

    #[test]
    fn assigning_an_undefined_variable_is_an_error() {
        assert_eq!(
            run_err("{ y = 1 }"),
            "cannot assign to undefined variable `y`"
        );
    }

    #[test]
    fn a_closure_sees_a_later_mutation_of_a_captured_mut_local() {
        // Capture-by-reference: `f` shares `n`'s cell, so the `n = 99` is visible.
        assert_eq!(
            run("{ let mut n = 0  let f = () -> n  n = 99  f() }"),
            Value::Int(99)
        );
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
        assert_eq!(
            run_err("1 => 10 | 20"),
            "condition requires a Bool, got Int"
        );
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
        assert_eq!(
            run("{ let n = 10  let add = x -> x + n  add(5) }"),
            Value::Int(15)
        );
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
        assert_eq!(
            run("{ let apply = g -> g(10)  apply($ + 1) }"),
            Value::Int(11)
        );
    }

    #[test]
    fn a_placeholder_gap_ignores_the_skipped_argument() {
        // `($b)` references only the second positional slot, so it desugars to
        // `(_, $b) -> $b` — a two-arg lambda that drops the first argument.
        assert_eq!(
            run("{ let apply = g -> g(10, 20)  apply($b) }"),
            Value::Int(20)
        );
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
            eval_program(&items).expect_err("should fail").message(),
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
            run_program(
                "prod Point(x: Int, y: Int)  main() = { let p = Point(1, 2)  Point(..p, x: 10).x }"
            ),
            Value::Int(10)
        );
        assert_eq!(
            run_program(
                "prod Point(x: Int, y: Int)  main() = { let p = Point(1, 2)  Point(..p, x: 10).y }"
            ),
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

    #[test]
    fn constructs_a_sum_variant_with_a_named_field() {
        assert_eq!(
            run_program(
                "sum Shape = Circle(radius: Int) | Rect(w: Int, h: Int)  main() = Circle(5).radius"
            ),
            Value::Int(5)
        );
    }

    #[test]
    fn nullary_variants_are_bare_values() {
        assert_eq!(
            run_program("sum Color = Red | Green | Blue  main() = Red == Red"),
            Value::Bool(true)
        );
        assert_eq!(
            run_program("sum Color = Red | Green | Blue  main() = Red == Green"),
            Value::Bool(false)
        );
    }

    #[test]
    fn variants_of_one_sum_are_distinguished_structurally() {
        assert_eq!(
            run_program("sum Opt = Just(Int) | Nothing  main() = Just(1) == Just(1)"),
            Value::Bool(true)
        );
        assert_eq!(
            run_program("sum Opt = Just(Int) | Nothing  main() = Just(1) == Nothing"),
            Value::Bool(false)
        );
    }

    #[test]
    fn evaluates_a_string_literal() {
        assert_eq!(run(r#""hello""#), Value::Str("hello".into()));
    }

    #[test]
    fn strings_have_structural_equality() {
        assert_eq!(run(r#""a" == "a""#), Value::Bool(true));
        assert_eq!(run(r#""a" == "b""#), Value::Bool(false));
    }

    #[test]
    fn interpolates_an_expression() {
        assert_eq!(run(r#""n is {1 + 2}""#), Value::Str("n is 3".into()));
    }

    #[test]
    fn interpolates_several_holes_and_literals() {
        assert_eq!(run(r#""{1}-{2}-{3}""#), Value::Str("1-2-3".into()));
    }

    #[test]
    fn interpolates_a_string_without_adding_quotes() {
        assert_eq!(
            run(r#"{ let name = "Bo"  "hi {name}!" }"#),
            Value::Str("hi Bo!".into())
        );
    }

    #[test]
    fn interpolation_renders_a_bool() {
        assert_eq!(run(r#""{true}""#), Value::Str("true".into()));
    }

    #[test]
    fn interpolation_renders_a_data_value() {
        assert_eq!(
            run_program(r#"sum Color = Red | Green | Blue  main() = "it is {Green}""#),
            Value::Str("it is Green".into())
        );
        assert_eq!(
            run_program(r#"prod Point(x: Int, y: Int)  main() = "{Point(1, 2)}""#),
            Value::Str("Point(x: 1, y: 2)".into())
        );
    }

    #[test]
    fn tuples_have_structural_equality() {
        assert_eq!(run("(1, 2) == (1, 2)"), Value::Bool(true));
        assert_eq!(run("(1, 2) == (1, 3)"), Value::Bool(false));
    }

    #[test]
    fn the_empty_tuple_is_unit() {
        assert_eq!(run("()"), Value::Unit);
    }

    #[test]
    fn interpolation_renders_a_tuple() {
        assert_eq!(run(r#""{(1, 2)}""#), Value::Str("(1, 2)".into()));
    }

    #[test]
    fn plus_concatenates_two_strings() {
        assert_eq!(run(r#""foo" + "bar""#), Value::Str("foobar".into()));
    }

    #[test]
    fn plus_across_string_and_number_is_a_type_error() {
        assert_eq!(
            run_err(r#""a" + 1"#),
            "operator Add cannot apply to Str and Int"
        );
    }

    #[test]
    fn lists_have_structural_equality() {
        assert_eq!(run("[1, 2, 3] == [1, 2, 3]"), Value::Bool(true));
        assert_eq!(run("[1, 2, 3] == [1, 2, 4]"), Value::Bool(false));
        assert_eq!(run("[] == []"), Value::Bool(true));
    }

    #[test]
    fn a_list_can_hold_computed_elements() {
        assert_eq!(run("[1 + 1, 2 * 3] == [2, 6]"), Value::Bool(true));
    }

    #[test]
    fn interpolation_renders_a_list() {
        assert_eq!(run(r#""{[1, 2, 3]}""#), Value::Str("[1, 2, 3]".into()));
    }

    #[test]
    fn maps_have_order_insensitive_structural_equality() {
        assert_eq!(
            run(r#"["a": 1, "b": 2] == ["b": 2, "a": 1]"#),
            Value::Bool(true)
        );
        assert_eq!(run(r#"["a": 1] == ["a": 2]"#), Value::Bool(false));
        assert_eq!(run("[:] == [:]"), Value::Bool(true));
    }

    #[test]
    fn a_later_duplicate_map_key_wins() {
        assert_eq!(run(r#"["a": 1, "a": 9] == ["a": 9]"#), Value::Bool(true));
    }

    #[test]
    fn interpolation_renders_a_map() {
        assert_eq!(
            run(r#""{["a": 1, "b": 2]}""#),
            Value::Str("[a: 1, b: 2]".into())
        );
        assert_eq!(run(r#""{[:]}""#), Value::Str("[:]".into()));
    }

    #[test]
    fn indexing_a_map_returns_some_or_none() {
        assert_eq!(
            run(r#"match ["a": 1, "b": 2]["a"] { Some(v) => v  None => 0 }"#),
            Value::Int(1)
        );
        assert_eq!(
            run(r#"match ["a": 1, "b": 2]["z"] { Some(v) => v  None => 0 }"#),
            Value::Int(0)
        );
    }

    #[test]
    fn maps_can_be_keyed_by_any_value() {
        assert_eq!(
            run(r#"match [1: "x", 2: "y"][2] { Some(v) => v  None => "?" }"#),
            Value::Str("y".into())
        );
    }

    #[test]
    fn indexing_a_list_returns_some_or_none() {
        assert_eq!(
            run("match [10, 20, 30][1] { Some(v) => v  None => -1 }"),
            Value::Int(20)
        );
        assert_eq!(
            run("match [10, 20, 30][9] { Some(v) => v  None => -1 }"),
            Value::Int(-1)
        );
    }

    #[test]
    fn indexing_with_safe_nav_chains() {
        // `m[k]` is a Maybe, so `?.` flows straight on.
        assert_eq!(
            run_program(
                r#"prod Pt(x: Int)  main() = match ["p": Pt(7)]["p"]?.x { Some(v) => v  None => 0 }"#
            ),
            Value::Int(7)
        );
    }

    #[test]
    fn indexing_a_non_collection_is_an_error() {
        assert_eq!(run_err("5[0]"), "cannot index a Int");
    }

    #[test]
    fn a_non_int_list_index_is_an_error() {
        assert_eq!(
            run_err(r#"[1, 2, 3]["x"]"#),
            "list index must be an Int, got Str"
        );
    }

    #[test]
    fn pipe_inserts_the_left_as_the_first_argument() {
        // 10 |> sub(3)  ==  sub(10, 3)  ==  7
        assert_eq!(
            run_program("sub(a, b) = a - b  main() = 10 |> sub(3)"),
            Value::Int(7)
        );
    }

    #[test]
    fn pipe_into_a_bare_reference_calls_it() {
        assert_eq!(
            run_program("total(xs) = fold(xs, 0, (a, b) -> a + b)  main() = [1, 2, 3] |> total"),
            Value::Int(6)
        );
    }

    #[test]
    fn pipes_chain_left_to_right() {
        assert_eq!(
            run_program(
                "main() = [1, 2, 3, 4] |> filter(x -> x > 2) |> map(x -> x * 10) == [30, 40]"
            ),
            Value::Bool(true)
        );
    }

    #[test]
    fn use_makes_the_rest_of_the_block_the_callback() {
        // `use <- span("report")` ≡ `span("report", () -> { emit(...) })`
        assert_eq!(
            run_program_events(r#"main() = { use <- span("report")  emit("x", 1) }"#),
            vec![
                TelemetryEvent::SpanOpen {
                    name: "report".to_string()
                },
                TelemetryEvent::Emit {
                    name: "x".to_string(),
                    value: Value::Int(1)
                },
                TelemetryEvent::SpanClose {
                    name: "report".to_string()
                },
            ]
        );
    }

    #[test]
    fn use_binds_the_callback_parameter() {
        // `use n <- withTen()` ≡ `withTen(n -> { n + 1 })`; withTen(f) = f(10).
        assert_eq!(
            run_program("withTen(f) = f(10)  main() = { use n <- withTen()  n + 1 }"),
            Value::Int(11)
        );
    }

    #[test]
    fn maybe_is_built_in() {
        assert_eq!(
            run_program("main() = match Some(5) { Some(x) => x  None => 0 }"),
            Value::Int(5)
        );
        assert_eq!(
            run_program("main() = match None { Some(x) => x  None => 0 }"),
            Value::Int(0)
        );
    }

    #[test]
    fn result_is_built_in() {
        assert_eq!(
            run_program("main() = match Ok(7) { Ok(v) => v  Err(e) => 0 }"),
            Value::Int(7)
        );
        assert_eq!(
            run_program("main() = match Err(9) { Ok(v) => v  Err(e) => e }"),
            Value::Int(9)
        );
    }

    #[test]
    fn built_in_options_have_structural_equality() {
        assert_eq!(
            run_program("main() = Some(1) == Some(1)"),
            Value::Bool(true)
        );
        assert_eq!(run_program("main() = Some(1) == None"), Value::Bool(false));
    }

    #[test]
    fn try_unwraps_a_present_value() {
        assert_eq!(
            run_program(
                "f(m) = { let x = m?  Some(x + 1) }  main() = match f(Some(10)) { Some(v) => v  None => 0 }"
            ),
            Value::Int(11)
        );
    }

    #[test]
    fn try_short_circuits_on_none() {
        // `m?` on None returns None *from f* — so f(None) is None.
        assert_eq!(
            run_program(
                "f(m) = { let x = m?  Some(x + 1) }  main() = match f(None) { Some(v) => v  None => 0 }"
            ),
            Value::Int(0)
        );
    }

    #[test]
    fn try_propagates_err() {
        assert_eq!(
            run_program(
                "f(r) = { let x = r?  Ok(x) }  main() = match f(Err(5)) { Ok(v) => v  Err(e) => e }"
            ),
            Value::Int(5)
        );
    }

    #[test]
    fn try_returns_from_the_nearest_enclosing_function_only() {
        // inner short-circuits to None; outer keeps going and returns 999.
        let src = "inner(m) = { let x = m?  x * 10 }  \
                   outer() = { let a = inner(None)  999 }  \
                   main() = outer()";
        assert_eq!(run_program(src), Value::Int(999));
    }

    #[test]
    fn try_on_a_non_option_is_an_error() {
        assert_eq!(
            run_program_err("main() = { let x = 5?  x }"),
            "`?` expects a value implementing `Try` (e.g. Maybe/Result), got Int"
        );
    }

    #[test]
    fn try_works_on_a_user_type_implementing_the_try_contract() {
        // The payoff of `?` being a contract: a domain type with a
        // success/failure split opts into short-circuiting by implementing
        // `isFailure`/`unwrap` — `?` bails on `Denied`, unwraps `Granted`.
        let src = "sum Perm = Granted(Int) | Denied  \
                   on Perm : Try { \
                       isFailure() = match @ { Denied => true  Granted(_) => false } \
                       unwrap() = match @ { Granted(n) => n  Denied => 0 } \
                   }  \
                   check(p) = { let n = p?  Granted(n + 1) }  \
                   main() = match check(Granted(10)) { Granted(n) => n  Denied => -1 }";
        assert_eq!(run_program(src), Value::Int(11));
    }

    #[test]
    fn try_short_circuits_on_a_user_failure_case() {
        let src = "sum Perm = Granted(Int) | Denied  \
                   on Perm : Try { \
                       isFailure() = match @ { Denied => true  Granted(_) => false } \
                       unwrap() = match @ { Granted(n) => n  Denied => 0 } \
                   }  \
                   check(p) = { let n = p?  Granted(n + 1) }  \
                   main() = match check(Denied) { Granted(n) => n  Denied => -1 }";
        assert_eq!(run_program(src), Value::Int(-1));
    }

    #[test]
    fn safe_nav_accesses_a_field_inside_some() {
        assert_eq!(
            run_program(
                r#"prod User(name: Str)  main() = match Some(User("Bo"))?.name { Some(n) => n  None => "?" }"#
            ),
            Value::Str("Bo".into())
        );
    }

    #[test]
    fn safe_nav_passes_none_through() {
        assert_eq!(
            run_program(r#"main() = match None?.name { Some(n) => n  None => "absent" }"#),
            Value::Str("absent".into())
        );
    }

    #[test]
    fn safe_nav_chains() {
        let src = "prod Addr(zip: Int)  prod User(addr: Addr)  \
                   main() = match Some(User(Addr(90210)))?.addr?.zip { Some(z) => z  None => 0 }";
        assert_eq!(run_program(src), Value::Int(90210));
    }

    #[test]
    fn safe_nav_passes_err_through() {
        assert_eq!(
            run_program("main() = match Err(7)?.name { Ok(v) => v  Err(e) => e }"),
            Value::Int(7)
        );
    }

    #[test]
    fn safe_nav_on_a_non_option_is_an_error() {
        assert_eq!(
            run_program_err("main() = 5?.x"),
            "`?.` expects a value implementing `Functor` (e.g. Maybe/Result), got Int"
        );
    }

    #[test]
    fn safe_nav_works_on_a_user_type_implementing_functor() {
        // The payoff of `?.` being a contract: a user container that implements
        // `map` gets safe-navigation. `Box(p)?.x` maps `.x` inside the Box.
        let src = "prod Pt(x: Int)  sum Box = Full(Pt) | Empty  \
                   on Box : Functor { map(f) = match @ { Full(v) => Full(f(v))  Empty => Empty } }  \
                   main() = match Full(Pt(7))?.x { Full(n) => n  Empty => 0 }";
        assert_eq!(run_program(src), Value::Int(7));
    }

    #[test]
    fn prelude_count_and_total() {
        assert_eq!(run_program("main() = count([1, 2, 3])"), Value::Int(3));
        assert_eq!(run_program("main() = total([1, 2, 3, 4])"), Value::Int(10));
    }

    #[test]
    fn prelude_any_all_contains() {
        assert_eq!(
            run_program("main() = any([1, 2, 3], x -> x > 2)"),
            Value::Bool(true)
        );
        assert_eq!(
            run_program("main() = all([1, 2, 3], x -> x > 2)"),
            Value::Bool(false)
        );
        assert_eq!(
            run_program("main() = contains([1, 2, 3], 2)"),
            Value::Bool(true)
        );
    }

    #[test]
    fn prelude_find_returns_first_match() {
        assert_eq!(
            run_program(
                "main() = match find([1, 2, 3, 4], x -> x > 2) { Some(v) => v  None => 0 }"
            ),
            Value::Int(3)
        );
    }

    #[test]
    fn prelude_each_runs_for_effect() {
        assert_eq!(
            run_program_events(r#"main() = each([1, 2, 3], x -> emit("n", x))"#),
            vec![
                TelemetryEvent::Emit {
                    name: "n".to_string(),
                    value: Value::Int(1)
                },
                TelemetryEvent::Emit {
                    name: "n".to_string(),
                    value: Value::Int(2)
                },
                TelemetryEvent::Emit {
                    name: "n".to_string(),
                    value: Value::Int(3)
                },
            ]
        );
    }

    #[test]
    fn prelude_maybe_helpers() {
        assert_eq!(run_program("main() = unwrapOr(None, 99)"), Value::Int(99));
        assert_eq!(run_program("main() = unwrapOr(Some(7), 99)"), Value::Int(7));
        assert_eq!(
            run_program(
                "main() = match andThen(Some(5), x -> Some(x + 1)) { Some(v) => v  None => 0 }"
            ),
            Value::Int(6)
        );
    }

    #[test]
    fn a_user_definition_shadows_the_prelude() {
        assert_eq!(
            run_program("count(xs) = 999  main() = count([1, 2, 3])"),
            Value::Int(999)
        );
    }

    #[test]
    fn a_report_style_program_runs_end_to_end() {
        // Records + field access + placeholder lambdas + pipes + combinators +
        // `use <- span` + `emit`, together — the shape of the canonical sample.
        let src = r#"
            prod Reading(sensor: Str, celsius: Int)
            report(readings) = {
                use <- span("report")
                let hot = readings |> filter($.celsius > 30) |> map($.celsius)
                emit("hot.count", fold(hot, 0, (acc, _) -> acc + 1))
            }
            main() = report([Reading("a", 35), Reading("b", 20), Reading("c", 40)])
        "#;
        assert_eq!(
            run_program_events(src),
            vec![
                TelemetryEvent::SpanOpen {
                    name: "report".to_string()
                },
                TelemetryEvent::Emit {
                    name: "hot.count".to_string(),
                    value: Value::Int(2)
                },
                TelemetryEvent::SpanClose {
                    name: "report".to_string()
                },
            ]
        );
    }

    // --- method dispatch via `on` blocks (basic `on X`, no contract) ---
    // RED until the dispatch lookup is wired into eval. The registry that
    // `register_items` builds is populated but not yet consulted, so these
    // exercise the path that turns `value.method(args)` into the right
    // `on`-block method call.

    #[test]
    fn dispatches_an_inherent_method() {
        // Pure dispatch: the method ignores the receiver, so this isolates
        // "find the method on the value's type and call it" with nothing else.
        assert_eq!(
            run_program("prod Box(n: Int)  on Box { label() = 42 }  main() = Box(7).label()"),
            Value::Int(42)
        );
    }

    #[test]
    fn a_method_reads_a_receiver_field() {
        // Adds receiver binding: `@n` must resolve to the called value's field.
        assert_eq!(
            run_program("prod Box(n: Int)  on Box { get() = @n }  main() = Box(7).get()"),
            Value::Int(7)
        );
    }

    #[test]
    fn a_method_takes_an_argument() {
        // Adds parameter passing alongside the receiver.
        assert_eq!(
            run_program("prod Box(n: Int)  on Box { plus(k) = @n + k }  main() = Box(7).plus(3)"),
            Value::Int(10)
        );
    }

    #[test]
    fn a_mut_method_mutates_the_receiver() {
        // `bump` writes `@n`; the change must persist back to the caller's `c`.
        assert_eq!(
            run_program(
                "prod Counter(mut n: Int)  on Counter { mut bump() { @n = @n + 1 } }  \
                 main() = { let mut c = Counter(0)  c.bump()  c.bump()  c.n }"
            ),
            Value::Int(2)
        );
    }

    #[test]
    fn a_mut_method_can_early_return_via_try() {
        // A `mut` body may use `?`; the early return stops at the method boundary
        // and mutations made before it still persist.
        assert_eq!(
            run_program(
                "prod Acct(mut bal: Int)  \
                 on Acct { mut withdraw(amount: Int) -> Result<(), Str> = { \
                   @bal = @bal - amount  Ok(()) } }  \
                 main() = { let mut a = Acct(100)  a.withdraw(30)  a.bal }"
            ),
            Value::Int(70)
        );
    }

    #[test]
    fn a_mut_method_on_a_temporary_is_an_error() {
        // No place to write the mutation back to.
        assert_eq!(
            run_program_err(
                "prod Counter(mut n: Int)  on Counter { mut bump() { @n = @n + 1 } }  \
                 main() = Counter(0).bump()"
            ),
            "cannot call mut method `bump` on a temporary — it has no place to write back to"
        );
    }

    #[test]
    fn a_mut_method_on_an_immutable_binding_is_an_error() {
        assert_eq!(
            run_program_err(
                "prod Counter(mut n: Int)  on Counter { mut bump() { @n = @n + 1 } }  \
                 main() = { let c = Counter(0)  c.bump()  c.n }"
            ),
            "cannot assign to immutable `c` (declare it with `let mut`)"
        );
    }

    #[test]
    fn a_contract_default_method_is_inherited() {
        // `describe` has a default body in the contract; Box conforms via
        // `on Box : Show` but doesn't override it, so the default runs.
        assert_eq!(
            run_program(
                "contract Show { show() -> Str  describe() = \"a thing\" }  \
                 prod Box(n: Int)  on Box : Show { show() = \"box\" }  \
                 main() = Box(1).describe()"
            ),
            Value::Str("a thing".into())
        );
    }

    #[test]
    fn a_contract_default_dispatches_self_calls_to_the_concrete_type() {
        // `loud` is a default that calls `@speak()`. Dog implements `speak`; the
        // default's `@speak()` must dispatch to Dog's impl — late binding / open
        // recursion (the template-method pattern).
        assert_eq!(
            run_program(
                "contract Voice { speak() -> Str  loud() = \"{@speak()}!\" }  \
                 prod Dog(x: Int)  on Dog : Voice { speak() = \"woof\" }  \
                 main() = Dog(1).loud()"
            ),
            Value::Str("woof!".into())
        );
    }

    #[test]
    fn a_concrete_method_overrides_a_contract_default() {
        // Box defines `hi`, so its impl wins over the contract's default body.
        assert_eq!(
            run_program(
                "contract Greet { hi() = \"default\" }  \
                 prod Box(n: Int)  on Box : Greet { hi() = \"box hi\" }  \
                 main() = Box(1).hi()"
            ),
            Value::Str("box hi".into())
        );
    }

    #[test]
    fn a_free_method_is_called_on_the_type() {
        // `free` methods take no receiver — call them on the type itself
        // (`Counter.zero()`), which resolves through the type's constructor.
        assert_eq!(
            run_program(
                "prod Counter(n: Int)  on Counter { free zero() -> Counter = Counter(0) }  \
                 main() = Counter.zero().n"
            ),
            Value::Int(0)
        );
    }

    #[test]
    fn an_instance_method_called_on_the_type_is_an_error() {
        // `get` needs a receiver; calling it on the type has no `@` to bind.
        assert_eq!(
            run_program_err(
                "prod Counter(n: Int)  on Counter { get() = @n }  main() = Counter.get()"
            ),
            "method `get` needs an instance receiver — call it on a value"
        );
    }

    #[test]
    fn a_free_method_called_on_an_instance_is_an_error() {
        // `free zero` belongs to the type, not an instance.
        assert_eq!(
            run_program_err(
                "prod Counter(n: Int)  on Counter { free zero() -> Counter = Counter(0) }  \
                 main() = Counter(5).zero()"
            ),
            "free method `zero` is called on the type `Counter`, not an instance"
        );
    }

    #[test]
    fn a_bare_sibling_call_does_not_resolve_to_a_method() {
        // The receiver is never implicit: a method reaches a sibling via
        // `@speak()`, not bare `speak()`. Bare names stay lexical/global, so a
        // method body that writes `speak()` finds no such binding.
        assert_eq!(
            run_program_err(
                "prod Dog(x: Int)  on Dog { speak() = \"woof\"  loud() = \"{speak()}!\" }  \
                 main() = Dog(1).loud()"
            ),
            "unbound variable `speak`"
        );
    }

    // --- adversarial probes: dispatch/field interactions (may surface gaps) ---

    #[test]
    fn calling_a_field_that_holds_a_function() {
        // `b.f(10)` where `f` is a field holding a closure. Intuitively this
        // reads the field and calls it. Does method-dispatch interception break
        // it (no method `f` exists)?
        assert_eq!(
            run_program(
                "prod Box(f: Int)  main() = { let b = Box(x -> x + 1)  b.f(10) }"
            ),
            Value::Int(11)
        );
    }

    #[test]
    fn a_field_and_a_method_can_share_a_name() {
        // `n` is both a field and a (zero-arg) method. `b.n` should read the
        // field; `b.n()` should call the method.
        assert_eq!(
            run_program(
                "prod Box(n: Int)  on Box { n() = 42 }  \
                 main() = { let b = Box(1)  b.n + b.n() }"
            ),
            Value::Int(43)
        );
    }

    #[test]
    fn self_reference_outside_a_method_is_an_error() {
        assert_eq!(
            run_program_err("main() = @"),
            "`@` is only valid inside a method body"
        );
    }

    #[test]
    fn a_free_method_on_a_sum_type() {
        // `free` methods are reached via the type name. A sum's type name isn't a
        // value (only its variants are), so `Maybe2.make()` resolves as a
        // type-path call (Java-style): the name is looked up as a type, not
        // evaluated as a value.
        assert_eq!(
            run_program(
                "sum Maybe2 = Yes(Int) | No  on Maybe2 { free make() -> Maybe2 = No }  \
                 main() = match Maybe2.make() { Yes(n) => n  No => 0 }"
            ),
            Value::Int(0)
        );
    }

    // --- edge-case probes (value semantics, nesting, sum dispatch, defaults) ---

    #[test]
    fn field_assignment_does_not_alias_a_copy() {
        // `b = a` copies (value semantics); mutating `b.x` must leave `a` alone.
        assert_eq!(
            run_program(
                "prod Point(mut x: Int, mut y: Int)  \
                 main() = { let mut a = Point(1, 2)  let mut b = a  b.x = 9  a.x }"
            ),
            Value::Int(1)
        );
    }

    #[test]
    fn a_mut_method_does_not_alias_a_copy() {
        // Same, through a mut method: bumping `b` must not touch `a`.
        assert_eq!(
            run_program(
                "prod Counter(mut n: Int)  on Counter { mut bump() { @n = @n + 1 } }  \
                 main() = { let mut a = Counter(0)  let mut b = a  b.bump()  a.n }"
            ),
            Value::Int(0)
        );
    }

    #[test]
    fn a_nested_field_can_be_assigned() {
        assert_eq!(
            run_program(
                "prod Inner(mut v: Int)  prod Outer(mut inner: Inner)  \
                 main() = { let mut o = Outer(Inner(1))  o.inner.v = 5  o.inner.v }"
            ),
            Value::Int(5)
        );
    }

    #[test]
    fn a_nested_assignment_through_an_immutable_field_is_an_error() {
        // `inner` isn't `mut`, so writing `o.inner.v` (which must rewrite
        // `o.inner`) is rejected even though `v` itself is `mut`.
        assert_eq!(
            run_program_err(
                "prod Inner(mut v: Int)  prod Outer(inner: Inner)  \
                 main() = { let mut o = Outer(Inner(1))  o.inner.v = 5  o.inner.v }"
            ),
            "cannot assign to immutable field `inner` of `Outer` (declare it `mut`)"
        );
    }

    #[test]
    fn a_mut_method_can_call_another_mut_method_on_self() {
        assert_eq!(
            run_program(
                "prod Counter(mut n: Int)  \
                 on Counter { mut bump() { @n = @n + 1 }  mut bumpTwice() { @bump()  @bump() } }  \
                 main() = { let mut c = Counter(0)  c.bumpTwice()  c.n }"
            ),
            Value::Int(2)
        );
    }

    #[test]
    fn a_method_dispatches_on_a_sum_variant() {
        assert_eq!(
            run_program(
                "sum Shape = Circle(r: Int) | Square(s: Int)  \
                 on Shape { name() -> Str = \"shape\" }  \
                 main() = Circle(5).name()"
            ),
            Value::Str("shape".into())
        );
    }

    #[test]
    fn an_inherent_method_beats_a_contract_default_in_a_separate_block() {
        assert_eq!(
            run_program(
                "contract Greet { hi() = \"default\" }  prod Box(n: Int)  \
                 on Box { hi() = \"inherent\" }  on Box : Greet { }  \
                 main() = Box(1).hi()"
            ),
            Value::Str("inherent".into())
        );
    }

    #[test]
    fn the_first_conforming_contract_supplies_a_clashing_default() {
        assert_eq!(
            run_program(
                "contract A { tag() = \"a\" }  contract B { tag() = \"b\" }  prod Box(n: Int)  \
                 on Box : A { }  on Box : B { }  main() = Box(1).tag()"
            ),
            Value::Str("a".into())
        );
    }

    #[test]
    fn calling_a_method_with_the_wrong_arity_is_an_error() {
        assert_eq!(
            run_program_err(
                "prod Box(n: Int)  on Box { plus(k) = @n + k }  main() = Box(7).plus(1, 2)"
            ),
            "method `plus` expects 1 argument(s), got 2"
        );
    }

    #[test]
    fn calling_an_unknown_method_is_an_error() {
        assert_eq!(
            run_program_err("prod Box(n: Int)  on Box { get() = @n }  main() = Box(7).missing()"),
            "Box has no method `missing`"
        );
    }

    #[test]
    fn methods_accumulate_across_multiple_on_blocks() {
        // The S5 insight: a type may have several `on` blocks; the registry
        // accumulates their methods rather than the later block clobbering the
        // earlier one. Both `get` and `double` must dispatch.
        assert_eq!(
            run_program(
                "prod Box(n: Int)  on Box { get() = @n }  on Box { double() = @n * 2 }  \
                 main() = Box(5).get() + Box(5).double()"
            ),
            Value::Int(15)
        );
    }
}
