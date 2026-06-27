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

use core::str;
use core::cell::{OnceCell, RefCell};

use alloc::collections::BTreeMap;

#[allow(clippy::wildcard_imports, reason = "alloc prelude for no_std")]
use crate::prelude::*;

use crate::ast::Method;
use crate::value::{TelemetryEvent, Value};

/// Why an assignment failed — formatted into a message by the interpreter.
pub enum AssignError {
    /// No binding of that name in scope.
    Unbound,
    /// The binding exists but wasn't declared `mut`.
    Immutable,
}

#[derive(Clone, Default)]
pub struct Env {
    locals: Option<Rc<Scope>>,
    globals: Rc<OnceCell<BTreeMap<String, Value>>>,
    methods: Rc<OnceCell<BTreeMap<String, Vec<Method>>>>,
    /// Per-variant field mutability: variant name → field name → is `mut`. The
    /// source of truth for whether a field may be assigned. (Keyed by variant so
    /// each sum variant's fields are tracked independently; for a `prod` the
    /// variant name is the type name.)
    field_mut: Rc<OnceCell<BTreeMap<String, BTreeMap<String, bool>>>>,
    /// Telemetry recorded by `emit`/`span`, shared across the whole program run
    /// (every scope and closure points at the same sink).
    sink: Rc<RefCell<Vec<TelemetryEvent>>>,
}

struct Scope {
    name: String,
    /// The binding's value lives in a shared cell, so a `mut` binding reassigned
    /// here is visible through every clone of this scope — including closures
    /// that captured it (capture-by-reference). Immutable bindings use a cell
    /// too, but `assign` refuses them, so it never changes.
    value: Rc<RefCell<Value>>,
    mutable: bool,
    parent: Option<Rc<Scope>>,
}

impl Env {
    /// The empty environment.
    pub fn new() -> Self {
        Env::default()
    }

    /// An environment sharing this one's globals, methods, and telemetry sink
    /// but with **no locals**. Used to run a top-level definition's body (a
    /// method, say) in global scope rather than the caller's lexical scope — the
    /// same hygiene a closure gets by capturing its own defining env instead of
    /// the caller's. Globals/methods stay reachable; the caller's locals don't
    /// leak in.
    #[must_use]
    pub fn globals_only(&self) -> Env {
        Env {
            locals: None,
            globals: Rc::clone(&self.globals),
            methods: Rc::clone(&self.methods),
            field_mut: Rc::clone(&self.field_mut),
            sink: Rc::clone(&self.sink),
        }
    }

    /// A sibling environment for another module: its own (fresh, not-yet-set)
    /// globals slot, but **sharing** this one's method/field-mutability tables and
    /// telemetry sink. Method dispatch is by runtime type name into one
    /// program-wide table, and telemetry is program-wide, so those are shared
    /// across every module; only the value namespace (`globals`) is per-module.
    #[must_use]
    pub fn sibling_module(&self) -> Env {
        Env {
            locals: None,
            globals: Rc::new(OnceCell::new()),
            methods: Rc::clone(&self.methods),
            field_mut: Rc::clone(&self.field_mut),
            sink: Rc::clone(&self.sink),
        }
    }

    /// A new environment with an immutable `name` binding, shadowing any
    /// existing binding and sharing the same globals + sink.
    #[must_use]
    pub fn extend(&self, name: String, value: Value) -> Env {
        self.bind(name, value, false)
    }

    /// As [`extend`](Self::extend), but the binding is `mut` (assignable).
    #[must_use]
    pub fn extend_mut(&self, name: String, value: Value) -> Env {
        self.bind(name, value, true)
    }

    fn bind(&self, name: String, value: Value, mutable: bool) -> Env {
        Env {
            locals: Some(Rc::new(Scope {
                name,
                value: Rc::new(RefCell::new(value)),
                mutable,
                parent: self.locals.clone(),
            })),
            globals: Rc::clone(&self.globals),
            methods: Rc::clone(&self.methods),
            field_mut: Rc::clone(&self.field_mut),
            sink: Rc::clone(&self.sink),
        }
    }

    /// Reassign an existing `mut` binding in place (mutating its shared cell, so
    /// the change is visible through every holder of this scope).
    ///
    /// # Errors
    /// `Unbound` if no such binding; `Immutable` if it isn't `mut`.
    pub fn assign(&self, name: &str, value: Value) -> Result<(), AssignError> {
        let mut current = &self.locals;
        while let Some(scope) = current {
            if scope.name == name {
                if !scope.mutable {
                    return Err(AssignError::Immutable);
                }
                *scope.value.borrow_mut() = value;
                return Ok(());
            }
            current = &scope.parent;
        }
        Err(AssignError::Unbound)
    }

    /// Record a telemetry event.
    pub fn emit(&self, event: TelemetryEvent) {
        self.sink.borrow_mut().push(event);
    }

    /// A snapshot of all telemetry recorded so far.
    pub fn telemetry(&self) -> Vec<TelemetryEvent> {
        self.sink.borrow().clone()
    }

    /// Drain the telemetry sink: return everything recorded and clear it. Lets a
    /// long-lived REPL env render just *this line's* events without the previous
    /// lines' accumulating.
    pub fn take_telemetry(&self) -> Vec<TelemetryEvent> {
        core::mem::take(&mut *self.sink.borrow_mut())
    }

    /// The value of the nearest local binding of `name`, else a global of that
    /// name, else `None`.
    pub fn lookup(&self, name: &str) -> Option<Value> {
        let mut current = &self.locals;
        while let Some(scope) = current {
            if scope.name == name {
                return Some(scope.value.borrow().clone());
            }
            current = &scope.parent;
        }
        self.globals
            .get()
            .and_then(|globals| globals.get(name).cloned())
    }

    pub fn lookup_method(&self, type_name: &str, method_name: &str) -> Option<Method> {
        self.methods
            .get()
            .and_then(|methods| {
                methods
                    .get(type_name)
                    .and_then(|for_type| for_type.iter().find(|method| method.name == method_name))
            })
            .cloned()
    }

    /// Whether any methods are registered for `type_name` — i.e. the name refers
    /// to a type with an `on` block. Lets a bare type name be recognised as a
    /// type-path receiver (`SumType.free_method()`) without it being a value.
    pub fn has_methods(&self, type_name: &str) -> bool {
        self.methods
            .get()
            .is_some_and(|methods| methods.contains_key(type_name))
    }

    /// Whether `field` of `variant` is declared `mut` — `None` if the variant
    /// has no such field. The source of truth for field-assignment legality.
    pub fn field_mutability(&self, variant: &str, field: &str) -> Option<bool> {
        self.field_mut
            .get()
            .and_then(|table| table.get(variant))
            .and_then(|fields| fields.get(field))
            .copied()
    }

    /// Install the program's top-level definitions into the shared table. Call
    /// exactly once, after building the closures that capture this env — they
    /// share the table, so each then sees every top-level definition.
    pub fn set_globals(&self, globals: BTreeMap<String, Value>) {
        assert!(
            self.globals.set(globals).is_ok(),
            "globals must be installed exactly once"
        );
    }

    pub fn set_methods(&self, methods: BTreeMap<String, Vec<Method>>) {
        assert!(
            self.methods.set(methods).is_ok(),
            "methods must be installed exactly once"
        );
    }

    pub fn set_field_mut(&self, field_mut: BTreeMap<String, BTreeMap<String, bool>>) {
        assert!(
            self.field_mut.set(field_mut).is_ok(),
            "field mutability must be installed exactly once"
        );
    }
}
