//! Lexical environment: an immutable chain of `name → Value` bindings plus a
//! shared, write-once table of top-level (global) definitions.
//!
//! Each `extend` returns a new `Env` that shares its tail (and the globals
//! slot) via `Rc`, so entering a scope — and capturing one in a closure — is
//! cheap and never mutates an existing binding. Lexical lookup walks the chain
//! from the most recent binding (so shadowing falls out for free); a miss falls
//! through to the globals. The globals are an `Rc<OnceCell<…>>` so that the
//! top-level functions, which all capture this env *before* the table is built,
//! end up sharing one fully-populated table — that shared table is what makes
//! recursion and mutual recursion work (letrec at the top level).

use std::cell::OnceCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::value::Value;

#[derive(Clone, Default)]
pub struct Env {
    locals: Option<Rc<Scope>>,
    globals: Rc<OnceCell<HashMap<String, Value>>>,
}

struct Scope {
    name: String,
    value: Value,
    parent: Option<Rc<Scope>>,
}

impl Env {
    /// The empty environment.
    pub fn new() -> Self {
        Env::default()
    }

    /// A new environment with `name` bound to `value`, shadowing any existing
    /// binding of the same name and sharing the same globals table.
    #[must_use]
    pub fn extend(&self, name: String, value: Value) -> Env {
        Env {
            locals: Some(Rc::new(Scope {
                name,
                value,
                parent: self.locals.clone(),
            })),
            globals: Rc::clone(&self.globals),
        }
    }

    /// The value of the nearest local binding of `name`, else a global of that
    /// name, else `None`.
    pub fn lookup(&self, name: &str) -> Option<Value> {
        let mut current = &self.locals;
        while let Some(scope) = current {
            if scope.name == name {
                return Some(scope.value.clone());
            }
            current = &scope.parent;
        }
        self.globals.get().and_then(|globals| globals.get(name).cloned())
    }

    /// Install the program's top-level definitions into the shared table. Call
    /// exactly once, after building the closures that capture this env — they
    /// share the table, so each then sees every top-level definition.
    pub fn set_globals(&self, globals: HashMap<String, Value>) {
        assert!(
            self.globals.set(globals).is_ok(),
            "globals must be installed exactly once"
        );
    }
}
