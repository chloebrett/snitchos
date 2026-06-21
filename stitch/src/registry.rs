//! Program registration: the setup phase that runs before evaluation. Walks the
//! top-level items and builds the tables `eval` needs — value globals (functions,
//! constructors, constants), per-type method lists, contract method tables, and
//! per-field mutability — then folds contract default methods into conformers.

use std::collections::HashMap;
use std::rc::Rc;

use crate::ast::{Field, Item, Method, Type};
use crate::env::Env;
use crate::value::{ClosureData, Constructor, DataValue, Value};

/// The top-level definitions collected from a program before they're installed
/// into the environment. Method dispatch needs more than the value `globals`:
/// per-type method lists, the contracts' own method tables (for default-method
/// bodies), and which contracts each type declares conformance to.
#[derive(Default)]
pub(crate) struct Registration {
    /// Value bindings: functions, constructors, top-level constants.
    pub(crate) globals: HashMap<String, Value>,
    /// Type name → its methods, gathered from every `on Type` block.
    pub(crate) methods: HashMap<String, Vec<Method>>,
    /// Contract name → its methods (abstract signatures and default bodies).
    contracts: HashMap<String, Vec<Method>>,
    /// Type name → the contracts it declares conformance to (`on Type : C`).
    conformances: HashMap<String, Vec<String>>,
    /// Variant name → field name → whether the field is declared `mut`. (Keyed
    /// by variant so each sum variant is independent; for a `prod` the variant
    /// name is the type name.)
    pub(crate) field_mut: HashMap<String, HashMap<String, bool>>,
}

/// Record the `mut` flag of each named field of `variant` into the registry.
/// Positional (unnamed) fields can't be assigned by name, so they're skipped.
fn register_field_mut(reg: &mut Registration, variant: &str, fields: &[Field]) {
    let entry = reg.field_mut.entry(variant.to_string()).or_default();
    for field in fields {
        if let Some(name) = &field.name {
            entry.insert(name.clone(), field.mutable);
        }
    }
}

/// Register each top-level item into `reg`. Functions and constructors capture
/// `env` so they share the (not-yet-filled) globals.
pub(crate) fn register_items(items: &[Item], env: &Env, reg: &mut Registration) {
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
                register_field_mut(reg, name, fields);
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
                    register_field_mut(reg, &variant.name, &variant.fields);
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
pub(crate) fn bake_contract_defaults(reg: &mut Registration) {
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
pub(crate) fn register_builtin_types(globals: &mut HashMap<String, Value>) {
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
