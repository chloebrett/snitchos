//! Tree-walk interpreter: recursively evaluate an `Expr` to a `Value`. The AST
//! *is* the program — no compilation. v0 is dynamically typed; see `value.rs`.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::rc::Rc;

use crate::ast::{
    Arg, BinOp, Expr, Item, MatchArm, Method, MethodModifier, Pattern, Stmt, StrSegment, Type, UnOp,
};
use crate::env::{AssignError, Env};
use crate::parser::parse_program;
use crate::value::{
    ClosureData, Constructor, DataValue, NativeFn, RuntimeError, TelemetryEvent, Value,
};

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
        Expr::Try(operand) => eval_try(eval(operand, env)?),
        Expr::SafeField { object, name } => eval_safe_field(eval(object, env)?, name),
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
    let receiver = eval(object, env)?;
    // The "receiver" expression is either an instance (`value.method()` → a
    // `Data`, which binds `@`) or the type itself (`Type.method()` → the type's
    // `Constructor`, for a `free`/associated method with no `@`). Both carry the
    // type name to dispatch on; anything else has no method table.
    let (type_name, self_value) = match &receiver {
        Value::Data(data) => (data.type_name.as_str(), Some(&receiver)),
        Value::Constructor(ctor) => (ctor.type_name.as_str(), None),
        _ => {
            return Err(RuntimeError::new(format!(
                "cannot call method `{name}` on {}",
                receiver.kind()
            )));
        }
    };
    let method = env
        .lookup_method(type_name, name)
        .ok_or_else(|| RuntimeError::new(format!("{type_name} has no method `{name}`")))?;

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

    // Arguments evaluate in the caller's scope; the body runs in global scope.
    // An instance method also binds the receiver as `@`; a `free` method doesn't.
    let mut method_env = env.globals_only();
    if let Some(receiver) = self_value {
        method_env = method_env.extend("@".to_string(), receiver.clone());
    }
    for (param, arg) in method.params.iter().zip(args) {
        method_env = method_env.extend(param.name.clone(), eval(&arg.value, env)?);
    }

    let body = method
        .body
        .as_ref()
        .expect("an `on`-block method always has a body (parser-enforced)");
    eval(body, &method_env)
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
fn apply_values(callee: &Value, args: &[Value], env: &Env) -> Result<Value, RuntimeError> {
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

/// The top-level definitions collected from a program before they're installed
/// into the environment. Method dispatch needs more than the value `globals`:
/// per-type method lists, the contracts' own method tables (for default-method
/// bodies), and which contracts each type declares conformance to.
#[derive(Default)]
struct Registration {
    /// Value bindings: functions, constructors, top-level constants.
    globals: HashMap<String, Value>,
    /// Type name → its methods, gathered from every `on Type` block.
    methods: HashMap<String, Vec<Method>>,
    /// Contract name → its methods (abstract signatures and default bodies).
    contracts: HashMap<String, Vec<Method>>,
    /// Type name → the contracts it declares conformance to (`on Type : C`).
    conformances: HashMap<String, Vec<String>>,
}

/// Register each top-level item into `reg`. Functions and constructors capture
/// `env` so they share the (not-yet-filled) globals.
fn register_items(items: &[Item], env: &Env, reg: &mut Registration) {
    for item in items {
        match item {
            Item::Func {
                name, params, body, ..
            } => {
                let closure = Value::Closure(Rc::new(ClosureData {
                    params: params.iter().map(|param| param.name.clone()).collect(),
                    body: body.clone(),
                    env: env.clone(),
                }));
                reg.globals.insert(name.clone(), closure);
            }
            Item::Prod { name, fields, .. } => {
                let ctor = Value::Constructor(Rc::new(Constructor {
                    type_name: name.clone(),
                    variant: name.clone(),
                    field_names: fields.iter().map(|field| field.name.clone()).collect(),
                }));
                reg.globals.insert(name.clone(), ctor);
            }
            Item::Sum { name, variants, .. } => {
                for variant in variants {
                    let value = if variant.fields.is_empty() {
                        // Nullary variant (`None`, `Red`) — a bare singleton value.
                        Value::Data(Rc::new(DataValue {
                            type_name: name.clone(),
                            variant: variant.name.clone(),
                            fields: Vec::new(),
                        }))
                    } else {
                        Value::Constructor(Rc::new(Constructor {
                            type_name: name.clone(),
                            variant: variant.name.clone(),
                            field_names: variant.fields.iter().map(|f| f.name.clone()).collect(),
                        }))
                    };
                    reg.globals.insert(variant.name.clone(), value);
                }
            }
            Item::On {
                target: Type::Name { name, .. },
                contract,
                methods,
            } => {
                reg.methods
                    .entry(name.clone())
                    .or_default()
                    .extend(methods.iter().cloned());
                // `on Type : Contract` records conformance, so the contract's
                // default methods can be folded in by `bake_contract_defaults`.
                if let Some(Type::Name { name: contract_name, .. }) = contract {
                    reg.conformances
                        .entry(name.clone())
                        .or_default()
                        .push(contract_name.clone());
                }
            }
            Item::Contract { name, methods, .. } => {
                reg.contracts.insert(name.clone(), methods.clone());
            }
            _ => {}
        }
    }
}

/// Fold each contract's default methods (those with a body) into every type that
/// declares conformance to it — unless the type already defines a method of that
/// name, in which case the concrete impl wins. Doing this once at registration
/// keeps `lookup_method` a single flat lookup; it's the same semantics as a
/// "method not found on the type → use the contract default" fallback at call
/// time. Abstract signatures (no body) are skipped — there's nothing to inherit.
fn bake_contract_defaults(reg: &mut Registration) {
    // Collect first, then apply: we can't mutate `reg.methods` while iterating
    // `reg.conformances`/`reg.contracts`.
    let mut additions: Vec<(String, Method)> = Vec::new();
    for (type_name, contract_names) in &reg.conformances {
        for contract_name in contract_names {
            let Some(contract_methods) = reg.contracts.get(contract_name) else {
                continue;
            };
            for method in contract_methods {
                if method.body.is_none() {
                    continue;
                }
                let defined_by_type = reg
                    .methods
                    .get(type_name)
                    .is_some_and(|ms| ms.iter().any(|m| m.name == method.name));
                // First contract to supply a given default wins (a later one with
                // the same method name is ignored once it's queued).
                let already_queued = additions
                    .iter()
                    .any(|(t, m)| t == type_name && m.name == method.name);
                if !defined_by_type && !already_queued {
                    additions.push((type_name.clone(), method.clone()));
                }
            }
        }
    }
    for (type_name, method) in additions {
        reg.methods.entry(type_name).or_default().push(method);
    }
}

/// Register the built-in `Maybe`/`Result` constructors: `Some`/`Ok`/`Err` take
/// one positional field; `None` is a bare singleton value. (User declarations
/// can still shadow these.)
fn register_builtin_types(globals: &mut HashMap<String, Value>) {
    let single_field = |type_name: &str, variant: &str| {
        Value::Constructor(Rc::new(Constructor {
            type_name: type_name.to_string(),
            variant: variant.to_string(),
            field_names: vec![None],
        }))
    };
    globals.insert("Some".to_string(), single_field("Maybe", "Some"));
    globals.insert("Ok".to_string(), single_field("Result", "Ok"));
    globals.insert("Err".to_string(), single_field("Result", "Err"));
    globals.insert(
        "None".to_string(),
        Value::Data(Rc::new(DataValue {
            type_name: "Maybe".to_string(),
            variant: "None".to_string(),
            fields: Vec::new(),
        })),
    );
}

/// The built-in (native) functions, registered into every program's globals.
const NATIVES: &[NativeFn] = &[
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
];

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

/// Evaluate a `match`: try each arm's pattern against `subject` in order; the
/// first that matches (and whose guard, if any, holds) wins. No arm matching is
/// a runtime error (v0 has no static exhaustiveness check yet).
fn eval_match(subject: &Value, arms: &[MatchArm], env: &Env) -> Result<Value, RuntimeError> {
    for arm in arms {
        let Some(bound) = try_match(&arm.pattern, subject, env) else {
            continue;
        };
        let guard_holds = match &arm.guard {
            Some(guard) => as_bool(&eval(guard, &bound)?, "match guard")?,
            None => true,
        };
        if guard_holds {
            return eval(&arm.body, &bound);
        }
    }
    Err(RuntimeError::new("no match arm matched"))
}

/// Try to match `pattern` against `value`. On success, return `env` extended
/// with the pattern's bindings; on failure, `None`. Each pattern kind is one
/// arm — constructor/or/tuple patterns arrive in later slices.
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

/// The `?` try operator: unwrap a present value (`Some`/`Ok`), or short-circuit
/// the enclosing function by returning the failure (`None`/`Err`) — signalled
/// via `RuntimeError::Return`, caught at the closure boundary in `apply_values`.
fn eval_try(value: Value) -> Result<Value, RuntimeError> {
    if let Value::Data(data) = &value {
        match (data.type_name.as_str(), data.variant.as_str()) {
            ("Maybe", "Some") | ("Result", "Ok") => {
                return Ok(data.fields.first().map_or(Value::Unit, |(_, v)| v.clone()));
            }
            ("Maybe", "None") | ("Result", "Err") => {
                return Err(RuntimeError::early_return(value));
            }
            _ => {}
        }
    }
    Err(RuntimeError::new(format!(
        "`?` expects a Maybe or Result, got {}",
        value.kind()
    )))
}

/// The `?.` safe-navigation operator: map a field access *inside* a `Maybe`/
/// `Result`. `Some(v)?.y` → `Some(v.y)`, `None?.y` → `None` (and likewise for
/// `Ok`/`Err`) — so a chain stays wrapped and short-circuits on the empty case.
fn eval_safe_field(object: Value, name: &str) -> Result<Value, RuntimeError> {
    if let Value::Data(data) = &object {
        match (data.type_name.as_str(), data.variant.as_str()) {
            // Empty/failure case: pass straight through, unchanged.
            ("Maybe", "None") | ("Result", "Err") => return Ok(object),
            // Present case: access the field, re-wrap in the same variant.
            ("Maybe", "Some") | ("Result", "Ok") => {
                let inner = data.fields.first().map_or(Value::Unit, |(_, v)| v.clone());
                let field = eval_field(&inner, name)?;
                return Ok(Value::Data(Rc::new(DataValue {
                    type_name: data.type_name.clone(),
                    variant: data.variant.clone(),
                    fields: vec![(None, field)],
                })));
            }
            _ => {}
        }
    }
    Err(RuntimeError::new(format!(
        "`?.` expects a Maybe or Result, got {}",
        object.kind()
    )))
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
                let Expr::Var(name) = target else {
                    return Err(RuntimeError::new(
                        "only a variable can be assigned (field assignment needs methods)",
                    ));
                };
                let new_value = eval(value, &scope)?;
                match scope.assign(name, new_value) {
                    Ok(()) => {}
                    Err(AssignError::Unbound) => {
                        return Err(RuntimeError::new(format!(
                            "cannot assign to undefined variable `{name}`"
                        )));
                    }
                    Err(AssignError::Immutable) => {
                        return Err(RuntimeError::new(format!(
                            "cannot assign to immutable `{name}` (declare it with `let mut`)"
                        )));
                    }
                }
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
    use crate::interp::{eval, eval_program, eval_program_with_telemetry};
    use crate::parser::{parse, parse_program};
    use crate::value::{TelemetryEvent, Value};

    /// Parse and run a program, returning the telemetry it emitted.
    fn run_program_events(src: &str) -> Vec<TelemetryEvent> {
        let items = parse_program(src).expect("test program should parse");
        let (result, events) = eval_program_with_telemetry(&items);
        result.expect("test program should evaluate");
        events
    }

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
            .message()
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
            .message()
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
    fn evaluates_a_string_literal() {
        assert_eq!(run(r#""hello""#), Value::Str("hello".into()));
    }

    #[test]
    fn strings_have_structural_equality() {
        assert_eq!(run(r#""a" == "a""#), Value::Bool(true));
        assert_eq!(run(r#""a" == "b""#), Value::Bool(false));
    }

    #[test]
    fn matches_a_string_literal_pattern() {
        assert_eq!(run(r#"match "hi" { "hi" => 1  _ => 0 }"#), Value::Int(1));
        assert_eq!(run(r#"match "yo" { "hi" => 1  _ => 0 }"#), Value::Int(0));
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
    fn map_applies_a_function_to_each_element() {
        assert_eq!(
            run_program("main() = map([1, 2, 3], x -> x * 2) == [2, 4, 6]"),
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
            "`?` expects a Maybe or Result, got Int"
        );
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
            "`?.` expects a Maybe or Result, got Int"
        );
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
