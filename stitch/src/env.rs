//! Lexical environment: an immutable chain of `name → Value` bindings. Each
//! `extend` returns a new `Env` that shares its tail via `Rc`, so entering a
//! scope (and, later, capturing one in a closure) is cheap and never mutates an
//! existing binding — a fit for immutable-by-default Stitch. Lookup walks from
//! the most recent binding, so shadowing falls out for free.

use std::rc::Rc;

use crate::value::Value;

#[derive(Clone, Default)]
pub struct Env(Option<Rc<Scope>>);

struct Scope {
    name: String,
    value: Value,
    parent: Env,
}

impl Env {
    /// The empty environment.
    pub fn new() -> Self {
        Env::default()
    }

    /// A new environment with `name` bound to `value`, shadowing any existing
    /// binding of the same name.
    #[must_use]
    pub fn extend(&self, name: String, value: Value) -> Env {
        Env(Some(Rc::new(Scope {
            name,
            value,
            parent: self.clone(),
        })))
    }

    /// The value of the nearest binding of `name`, or `None` if unbound.
    pub fn lookup(&self, name: &str) -> Option<Value> {
        let mut current = self;
        while let Some(scope) = &current.0 {
            if scope.name == name {
                return Some(scope.value.clone());
            }
            current = &scope.parent;
        }
        None
    }
}
