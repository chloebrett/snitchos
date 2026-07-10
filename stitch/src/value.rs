//! Runtime values and errors for the tree-walk interpreter. v0 is dynamically
//! typed: a `Value` carries its own kind, and operations check kinds at runtime
//! (no implicit Int/Float coercion — that previews the eventual static types).

use core::cell::{OnceCell, RefCell};
use core::fmt;

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::rc::Weak;

#[allow(clippy::wildcard_imports, reason = "alloc prelude for no_std")]
use crate::prelude::*;

use crate::core_ir::CoreExpr;
use crate::env::Env;
use crate::lexer::Span;

/// A value produced by evaluating an expression.
#[derive(Clone)]
pub enum Value {
    Int(i64),
    Float(f64),
    Bool(bool),
    /// An immutable string. `Rc<str>` so cloning a `Value::Str` is a refcount
    /// bump, not a copy.
    Str(Rc<str>),
    /// A tuple — the anonymous product `(a, b, …)`. The empty tuple `()` is
    /// `Unit`, not a zero-element `Tuple`. `Rc<[Value]>` for cheap clones.
    Tuple(Rc<[Value]>),
    /// An eager, finite, immutable list `[a, b, c]`. `Rc<[Value]>` for cheap
    /// clones; combinators produce fresh lists rather than mutating.
    List(Rc<[Value]>),
    /// An eager map `["k": v, …]` (empty is `[:]`). An assoc-list with unique
    /// keys: `Value` isn't hashable, and O(n) lookup is fine for the tree-walk
    /// stage. Equality is order-insensitive.
    Map(Rc<Vec<(Value, Value)>>),
    /// The unit value `()` — what a block with no trailing expression, and an
    /// expression evaluated only for effect, produce.
    Unit,
    /// A function: its parameters, body, and the environment it closed over,
    /// bundled into one shared heap object (cheap to pass around — cloning a
    /// `Value::Closure` just bumps the `Rc`).
    Closure(Rc<ClosureData>),
    /// A `prod`/variant constructor as a first-class value (callable to build a
    /// `Data`); produced by registering a `prod`/`sum` declaration.
    Constructor(Rc<Constructor>),
    /// A constructed `prod`/variant instance.
    Data(Rc<DataValue>),
    /// A built-in (native) function — the stdlib combinators (`map`, `filter`,
    /// …) implemented in Rust rather than in Stitch.
    Native(NativeFn),
    /// A lazy sequence — possibly infinite. A shared, memoizing cell that forces
    /// on demand to nil or head + lazy tail. The lazy counterpart to `List`.
    Seq(Rc<LazySeq>),
    /// A module: a named namespace of exported bindings, reached by path
    /// (`Module.member`). A first-class value so `import M` can bind `M` and
    /// `M.x` reuses the ordinary `.`-access dispatch. (Iteration 1 exports every
    /// top-level item; `pub` filtering arrives next.)
    Module(Rc<ModuleHandle>),
}

/// A module's exported bindings, keyed by name. Cloning a `Value::Module` shares
/// the handle (`Rc`); two module values are equal only if they are the same
/// module (identity, like a closure). `private` records names the module
/// *declares but does not export*, so a refused access can say "private" rather
/// than "no such member".
pub struct ModuleHandle {
    pub name: String,
    pub exports: BTreeMap<String, Value>,
    pub private: BTreeSet<String>,
}

impl ModuleHandle {
    /// The exported member named `name`, if any.
    pub fn member(&self, name: &str) -> Option<Value> {
        self.exports.get(name).cloned()
    }

    /// The error for an access that didn't resolve: a member declared but not
    /// exported is *private*; anything else simply doesn't exist.
    pub fn access_error(&self, name: &str) -> RuntimeError {
        if self.private.contains(name) {
            RuntimeError::new(format!(
                "member `{name}` of module `{}` is private",
                self.name
            ))
        } else {
            RuntimeError::new(format!("module `{}` has no member `{name}`", self.name))
        }
    }
}

/// A lazy sequence cell: forced on demand, then memoized. Cloning a
/// `Value::Seq` shares the cell (`Rc`), so forcing through one clone is visible
/// through all — a forced step is computed at most once.
pub struct LazySeq {
    cell: core::cell::RefCell<SeqState>,
}

enum SeqState {
    /// Not yet forced: run this thunk to produce the next step. The thunk holds
    /// Rust logic (it may call back into the interpreter for Stitch closures),
    /// so it is a boxed `Fn`, not a Stitch closure.
    Unforced(ForceFn),
    /// Forced and memoized.
    Forced(Step),
}

/// The thunk that produces a sequence's next step. `Rc<dyn Fn>` so a `LazySeq`
/// (and thus a `Value`) stays `Clone`.
type ForceFn = Rc<dyn Fn() -> Result<Step, RuntimeError>>;

/// One forced step of a sequence: the end, or a head plus the lazy tail (itself
/// a `Value::Seq`).
#[derive(Clone)]
pub enum Step {
    Nil,
    Cons(Value, Value),
}

impl LazySeq {
    /// A lazy cell whose first force runs `thunk`.
    pub fn new(thunk: impl Fn() -> Result<Step, RuntimeError> + 'static) -> Rc<Self> {
        Rc::new(LazySeq {
            cell: core::cell::RefCell::new(SeqState::Unforced(Rc::new(thunk))),
        })
    }

    /// Force the next step, computing it once and caching the result.
    pub fn force(&self) -> Result<Step, RuntimeError> {
        // Pull the thunk out (cloning the `Rc`) before calling it, so the cell
        // isn't borrowed while the thunk runs (it may force other sequences).
        let thunk = match &*self.cell.borrow() {
            SeqState::Forced(step) => return Ok(step.clone()),
            SeqState::Unforced(thunk) => Rc::clone(thunk),
        };
        let step = thunk()?;
        *self.cell.borrow_mut() = SeqState::Forced(step.clone());
        Ok(step)
    }
}

/// A built-in function: its name, arity, and the Rust implementation. The
/// implementation receives already-evaluated arguments plus the environment
/// (so it can reach the telemetry sink and apply function arguments), and may
/// call back into the interpreter (e.g. `map` applies its function to each
/// element; `span` runs a thunk).
#[derive(Clone, Copy)]
pub struct NativeFn {
    pub name: &'static str,
    pub arity: usize,
    pub func: fn(&[Value], &Env) -> Result<Value, RuntimeError>,
    /// The stdlib module this native is exported through, if any (e.g. `"Str"`).
    /// Drives `is_builtin_module` and `builtin_modules` — no separate spec table.
    pub module: Option<&'static str>,
    /// The name this native is exported under in its module. `None` means use `name`.
    /// Used when the internal name is prefixed to avoid flat-namespace collisions
    /// (e.g. `strUpper` → `upper` in `Str`).
    pub export_as: Option<&'static str>,
}

/// A telemetry event recorded by `emit`/`span`. The v0 stub for the real
/// `Frame` wire protocol — collected in a sink so the runtime (and tests) can
/// observe what a program reported.
#[derive(Debug, Clone, PartialEq)]
pub enum TelemetryEvent {
    SpanOpen { name: String },
    SpanClose { name: String },
    Emit { name: String, value: Value },
}

/// A constructor: which type/variant it builds and the names of its fields (in
/// declaration order; `None` for a positional field).
pub struct Constructor {
    pub type_name: String,
    pub variant: String,
    pub field_names: Vec<Option<String>>,
}

impl Constructor {
    /// The position of the field named `label`, if any.
    pub fn field_index(&self, label: &str) -> Option<usize> {
        self.field_names
            .iter()
            .position(|name| name.as_deref() == Some(label))
    }
}

/// A constructed value: its type and variant, and its fields in declaration
/// order paired with their declared names (`None` if positional).
pub struct DataValue {
    pub type_name: String,
    pub variant: String,
    pub fields: Vec<(Option<String>, Value)>,
    /// Provenance: `true` only for records the kernel/native code built (e.g.
    /// `hold`'s capability rows), which user Stitch can never construct — there
    /// is no surface syntax that sets it. This is the un-forgeable signal the
    /// renderer keys color on, so a user's `prod X(rights: Str)` can never be
    /// mistaken for real rights. Not part of value equality (it's provenance, not
    /// content), so a user record and a native one still compare equal by fields.
    pub native: bool,
}

/// The captured contents of a closure: parameter names, the body to evaluate on
/// each call, and the lexical environment in effect where the lambda appeared.
pub struct ClosureData {
    pub params: Vec<String>,
    /// The closure's code, as a shared reference into the core IR — closures over
    /// the same definition share this `Rc` instead of deep-cloning the body.
    pub body: Rc<CoreExpr>,
    /// The local bindings this closure closes over — captured at creation time
    /// as shared `Rc<RefCell<Value>>` cells (not value copies) so that `mut`
    /// bindings remain observable through the closure after reassignment.
    /// Only locals are stored here; globals are resolved via the closure's own
    /// home globals at call time, which breaks the `Rc<OnceCell> → globals map →
    /// Closure → Rc<OnceCell>` cycle (closures hold a `Weak`, not a strong ref).
    pub upvalues: Vec<(String, Rc<RefCell<Value>>, bool)>,
    /// A weak reference to the globals cell from the env in which this closure
    /// was defined ("home globals"). Stored as `Weak` to break the Rc cycle: the
    /// strong `Rc<OnceCell>` lives only in the env; when the env is dropped the
    /// map is freed even though closures still hold `Weak` refs. At call time we
    /// upgrade (always succeeds during a live `eval_program` call).
    pub home_globals: Weak<OnceCell<BTreeMap<String, Value>>>,
    /// The defining scope's authority, captured for lambdas (`uses: None`) so
    /// that capability constraints from the creation context are preserved.
    /// Named functions (`uses: Some(…)`) always override this at call time.
    pub authority: Rc<BTreeSet<String>>,
    /// The capability/effects clause for a *named function* (`Some`, possibly
    /// empty) — its body runs with exactly these authorities, not the caller's.
    /// `None` for a lambda, which inherits the authority of where it was defined.
    pub uses: Option<Vec<String>>,
}

impl Value {
    /// User-facing text for the value, as produced by string interpolation
    /// (and, later, `emit`/print). Distinct from `Debug`: strings render
    /// without quotes, data renders as `Variant(field, …)`. (A user-overridable
    /// `Show` contract is the eventual home for this.)
    pub fn display(&self) -> String {
        match self {
            Value::Int(n) => n.to_string(),
            Value::Float(x) => x.to_string(),
            Value::Bool(b) => b.to_string(),
            Value::Str(s) => s.to_string(),
            Value::Tuple(elements) => {
                let parts = elements
                    .iter()
                    .map(Value::display)
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("({parts})")
            }
            Value::List(elements) => {
                let parts = elements
                    .iter()
                    .map(Value::display)
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("[{parts}]")
            }
            Value::Map(entries) if entries.is_empty() => "[:]".to_string(),
            Value::Map(entries) => {
                let parts = entries
                    .iter()
                    .map(|(key, value)| format!("{}: {}", key.display(), value.display()))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("[{parts}]")
            }
            Value::Unit => "()".to_string(),
            Value::Closure(_) => "<function>".to_string(),
            Value::Constructor(_) => "<constructor>".to_string(),
            Value::Native(n) => format!("<builtin {}>", n.name),
            // Don't force — a Seq may be infinite.
            Value::Seq(_) => "<seq>".to_string(),
            Value::Module(m) => format!("<module {}>", m.name),
            Value::Data(d) if d.fields.is_empty() => d.variant.clone(),
            Value::Data(d) => {
                let fields = d
                    .fields
                    .iter()
                    .map(|(name, value)| match name {
                        Some(name) => format!("{name}: {}", value.display()),
                        None => value.display(),
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{}({fields})", d.variant)
            }
        }
    }

    /// The kind name, for error messages (`"Int"`, `"Function"`, …).
    pub fn kind(&self) -> &'static str {
        match self {
            Value::Int(_) => "Int",
            Value::Float(_) => "Float",
            Value::Bool(_) => "Bool",
            Value::Str(_) => "Str",
            Value::Tuple(_) => "Tuple",
            Value::List(_) => "List",
            Value::Map(_) => "Map",
            Value::Unit => "Unit",
            Value::Closure(_) | Value::Constructor(_) | Value::Native(_) => "Function",
            Value::Data(_) => "a record",
            Value::Seq(_) => "Seq",
            Value::Module(_) => "Module",
        }
    }
}

impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Int(n) => write!(f, "Int({n})"),
            Value::Float(x) => write!(f, "Float({x})"),
            Value::Bool(b) => write!(f, "Bool({b})"),
            Value::Str(s) => write!(f, "Str({s:?})"),
            Value::Tuple(elements) => write!(f, "Tuple{elements:?}"),
            Value::List(elements) => write!(f, "List{elements:?}"),
            Value::Map(entries) => write!(f, "Map{entries:?}"),
            Value::Unit => write!(f, "Unit"),
            Value::Closure(c) => write!(f, "Closure/{}", c.params.len()),
            Value::Constructor(c) => write!(f, "Constructor({})", c.variant),
            Value::Data(d) => write!(f, "{}{:?}", d.variant, d.fields),
            Value::Native(n) => write!(f, "Native({})", n.name),
            Value::Seq(_) => write!(f, "Seq"),
            Value::Module(m) => write!(f, "Module({})", m.name),
        }
    }
}

impl PartialEq for Value {
    /// Primitives compare by value; functions compare by identity (two closures
    /// are equal only if they are the same object) — functions have no
    /// structural equality.
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Float(a), Value::Float(b)) => a == b,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Str(a), Value::Str(b)) => a == b,
            (Value::Tuple(a), Value::Tuple(b)) | (Value::List(a), Value::List(b)) => a == b,
            // Maps are unordered: equal iff same size and every entry matches.
            (Value::Map(a), Value::Map(b)) => {
                a.len() == b.len()
                    && a.iter()
                        .all(|(key, value)| b.iter().any(|(k, v)| k == key && v == value))
            }
            (Value::Unit, Value::Unit) => true,
            (Value::Closure(a), Value::Closure(b)) => Rc::ptr_eq(a, b),
            (Value::Constructor(a), Value::Constructor(b)) => Rc::ptr_eq(a, b),
            (Value::Native(a), Value::Native(b)) => a.name == b.name,
            // Sequences are lazy/opaque — identity, like functions (no forcing).
            (Value::Seq(a), Value::Seq(b)) => Rc::ptr_eq(a, b),
            // A module is a namespace, not data — identity equality.
            (Value::Module(a), Value::Module(b)) => Rc::ptr_eq(a, b),
            // Structural equality (decision D): same type, variant, and fields.
            (Value::Data(a), Value::Data(b)) => {
                a.type_name == b.type_name && a.variant == b.variant && a.fields == b.fields
            }
            _ => false,
        }
    }
}

/// The error channel of evaluation. Carries either a real fault or a control
/// signal — `?`'s early return — which unwinds to the enclosing function rather
/// than aborting the program. (Reusing the `Err` channel for control flow is
/// the standard tree-walk technique for non-local return.)
#[derive(Debug, PartialEq)]
pub enum RuntimeError {
    /// A genuine runtime error (type mismatch, division by zero, …). `at` is the
    /// source span of the offending expression, stamped by `eval` as the fault
    /// propagates out (the innermost node wins); `None` until stamped.
    Fault { message: String, at: Option<Span> },
    /// `?` short-circuit: unwind to the enclosing function and return this value.
    Return(Value),
    /// Self-tail-call signal: `apply_values` catches this and loops instead of
    /// recursing, bounding the Rust stack for tail-recursive Stitch functions.
    TailCall(Vec<Value>),
}

impl RuntimeError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        RuntimeError::Fault { message: message.into(), at: None }
    }

    /// The `?` early-return control signal carrying the failure value.
    pub(crate) fn early_return(value: Value) -> Self {
        RuntimeError::Return(value)
    }

    /// Attach `span` to an unlocated fault — used by `eval` to stamp the span of
    /// the expression being evaluated when a fault first surfaces. Already-located
    /// faults (an inner node already stamped) and control signals pass through
    /// unchanged, so the innermost expression's span wins.
    #[must_use]
    pub(crate) fn stamped(self, span: Span) -> Self {
        match self {
            RuntimeError::Fault { message, at: None } => {
                RuntimeError::Fault { message, at: Some(span) }
            }
            other => other,
        }
    }

    /// The source span of a located fault, if any. `None` for control signals and
    /// for a fault that never reached an `eval` boundary to be stamped.
    #[must_use]
    pub fn span(&self) -> Option<Span> {
        match self {
            RuntimeError::Fault { at, .. } => *at,
            _ => None,
        }
    }

    /// The fault message for display. A `Return` reaching here means a `?` was
    /// used outside any function — surfaced as a fault rather than silently lost.
    pub fn message(&self) -> String {
        match self {
            RuntimeError::Fault { message, .. } => message.clone(),
            RuntimeError::Return(_) => "`?` used outside a function".to_string(),
            RuntimeError::TailCall(_) => {
                "internal: tail-call signal escaped the trampoline".to_string()
            }
        }
    }
}
