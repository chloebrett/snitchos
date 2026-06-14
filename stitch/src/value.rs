//! Runtime values and errors for the tree-walk interpreter. v0 is dynamically
//! typed: a `Value` carries its own kind, and operations check kinds at runtime
//! (no implicit Int/Float coercion — that previews the eventual static types).

use std::fmt;
use std::rc::Rc;

use crate::ast::Expr;
use crate::env::Env;

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
}

/// A built-in function: its name, arity, and the Rust implementation. The
/// implementation receives already-evaluated arguments and may call back into
/// the interpreter (e.g. `map` applies its function argument to each element).
#[derive(Clone, Copy)]
pub struct NativeFn {
    pub name: &'static str,
    pub arity: usize,
    pub func: fn(&[Value]) -> Result<Value, RuntimeError>,
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
}

/// The captured contents of a closure: parameter names, the body to evaluate on
/// each call, and the lexical environment in effect where the lambda appeared.
pub struct ClosureData {
    pub params: Vec<String>,
    pub body: Expr,
    pub env: Env,
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
            Value::Unit => "()".to_string(),
            Value::Closure(_) => "<function>".to_string(),
            Value::Constructor(_) => "<constructor>".to_string(),
            Value::Native(n) => format!("<builtin {}>", n.name),
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
            Value::Unit => "Unit",
            Value::Closure(_) | Value::Constructor(_) | Value::Native(_) => "Function",
            Value::Data(_) => "a record",
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
            Value::Unit => write!(f, "Unit"),
            Value::Closure(c) => write!(f, "Closure/{}", c.params.len()),
            Value::Constructor(c) => write!(f, "Constructor({})", c.variant),
            Value::Data(d) => write!(f, "{}{:?}", d.variant, d.fields),
            Value::Native(n) => write!(f, "Native({})", n.name),
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
            (Value::Unit, Value::Unit) => true,
            (Value::Closure(a), Value::Closure(b)) => Rc::ptr_eq(a, b),
            (Value::Constructor(a), Value::Constructor(b)) => Rc::ptr_eq(a, b),
            (Value::Native(a), Value::Native(b)) => a.name == b.name,
            // Structural equality (decision D): same type, variant, and fields.
            (Value::Data(a), Value::Data(b)) => {
                a.type_name == b.type_name && a.variant == b.variant && a.fields == b.fields
            }
            _ => false,
        }
    }
}

/// A runtime (evaluation) error. Carries a human-readable message, mirroring
/// `ParseError`; structured variants can come later if call sites need them.
#[derive(Debug, PartialEq)]
pub struct RuntimeError {
    pub message: String,
}

impl RuntimeError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}
