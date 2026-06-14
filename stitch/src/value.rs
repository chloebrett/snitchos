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
    /// The kind name, for error messages (`"Int"`, `"Function"`, …).
    pub fn kind(&self) -> &'static str {
        match self {
            Value::Int(_) => "Int",
            Value::Float(_) => "Float",
            Value::Bool(_) => "Bool",
            Value::Unit => "Unit",
            Value::Closure(_) | Value::Constructor(_) => "Function",
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
            Value::Unit => write!(f, "Unit"),
            Value::Closure(c) => write!(f, "Closure/{}", c.params.len()),
            Value::Constructor(c) => write!(f, "Constructor({})", c.variant),
            Value::Data(d) => write!(f, "{}{:?}", d.variant, d.fields),
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
            (Value::Unit, Value::Unit) => true,
            (Value::Closure(a), Value::Closure(b)) => Rc::ptr_eq(a, b),
            (Value::Constructor(a), Value::Constructor(b)) => Rc::ptr_eq(a, b),
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
